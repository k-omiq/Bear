//! `runClean`, `cancelRun`, `resumeJob` - and the four run events.
//!
//! The core already cleans a page end to end; `spikes/clean-page` is the whole
//! of that in about 150 lines. What was missing is everything around it: a
//! queue, a manifest to write into, a cancellation that keeps what it has, and
//! the ordering makes a contract rather than a courtesy.
//!
//! ## The shape: a scheduler and a cleaner, separately
//!
//! Every ordering rule the seam states is a property of the **scheduler** -
//! when events go out, how many, in what order, and what the manifest holds
//! when they do. None of them is a property of the pipeline that turns pixels
//! into patches. So the two are separated by [`Cleaner`]: [`Pipeline`] is the
//! real one and holds three ONNX sessions, and the scheduler is written against
//! the trait.
//!
//! That is not a testing convenience bolted on. It is what makes the ordering
//! rules testable *at all* on a machine without the weights, and it means the
//! tests exercise the same scheduler the window runs, with a different last
//! inch. The pipeline's own correctness is pinned elsewhere - by the core's
//! unit tests, and end to end by `spikes/clean-page`, which asserts the
//! fidelity contract against the file it wrote.
//!
//! ## Sessions are held; regions are never batched
//!
//! [`Pipeline`] opens the detector,
//! the balloon detector and the script gate **once per run** and carries them
//! across every page. There is no batching path here and there should not be
//! one: a run is a sequence of pages, each of which is a sequence of regions,
//! and each region's result is flushed before the next one starts.
//!
//! Rung 2's session is held the same way and opened differently: **not until a
//! region actually routes to it**, because it costs 2.1 s to build and about
//! 510 MB to hold, and a chapter of flat-paper dialogue never reaches it. See
//! [`Rung2`].
//!
//! ## The ladder is a loop, and its bottom rung is "leave it alone"
//!
//! The decline outcome is the state
//! where "no engine's output passes the uniform quality metric" - which is a
//! loop over rungs and not a property of one of them. [`clean_region`]
//! is that loop: the routed rung runs, [`quality::assess`] scores what came
//! back, and a decline escalates to the next rung [`ladder_from`] permits. When
//! the top permitted rung is declined too, the region is **left exactly as it
//! was** and listed in review with the reason - never half-cleaned, and never
//! written with the output of a rung that already failed. Leaving text is
//! recoverable; a bad fill is not.
//!
//! ## Auto clean is local-only
//!
//! It is enforced in one place: [`effective_ceiling`] never answers
//! [`Engine::Cloud`], whatever it is asked for. `needs-confirmation` lives on
//! `applyTool` and `runClean` has no equivalent, so a batch run at the cloud
//! ceiling would spend with neither the transmission statement nor the cost
//! confirmation. If cloud batch runs are ever wanted, this grows the
//! confirmation protocol **first**.
//!
//! And `engineCeiling` **caps, never raises**: both the stored setting and the
//! caller's argument can only lower the rung, never lift it.
//!
//! ## Detection may use the cloud, with its own consent
//!
//! RT-DETR v2 full and SAM-TS-L can run on the user's own cloud GPU instead
//! of this machine (`analysisTargets`). That is detection, not a rung: the
//! ceiling above still never reaches [`Engine::Cloud`]. It is also not
//! silent. A run that routes a stage to the cloud starts only with a run grant
//! the user confirmed for exactly this chapter, these pages and these models
//! ([`crate::inference::run_analysis`]); without one `run_clean` refuses with a
//! typed error before any model loads. A page whose cloud analysis fails fails
//! with a notice: it is neither retried nor analyzed locally instead.
//! CTD and Ogkalu Small have no cloud stage, so selecting either with cloud
//! detection refuses the run before any page is sent.
//!
//! The cloud analysis runs ahead of the cleaning rather than inside it: the run
//! starts a producer that analyzes its pages in order
//! ([`crate::inference::run_analysis::Prefetch`]), and each page takes its answer
//! when the run reaches it, so the cloud GPU is done, and released, long before
//! this computer is. A failure is still reported at its own page.
//!
//! ## What is written down, and when
//!
//! The "flushed after every
//! completed region" rule is [`Job::complete_region`]'s reason for existing, and
//! this module calls nothing else to record one. A page is marked
//! [examined](cleaner_core::project::Project::examined) **after** its regions
//! are recorded, so a crash between the two repeats one page rather than
//! skipping it. A page that could not be written is marked
//! [errored](cleaner_core::project::Project::errored) instead of examined, so it
//! is still the next run's work.
//!
//! ## One writer per job, in every process
//!
//! A manifest is rewritten whole after every region, and the run holds one open
//! for a whole chapter - so the span in which another command could interleave
//! on it went from milliseconds to the length of a run. [`lock_job`] closes
//! that for this process's threads and for every other process over the same
//! library, the GUI and a headless tool included: every read-modify-write of a
//! manifest in this crate takes it, and [`Job::flush`] refuses a manifest that
//! moved since it was read.
//!
//! ## Nothing gets out of here without a `run-finished`
//!
//! The seam's "exactly one per run, whatever happened" includes a panic in the
//! pipeline, so [`execute`]'s walk runs under
//! [`catch_unwind`](std::panic::catch_unwind) and a panicked run reports itself
//! cancelled at the page it stopped on. The active slot is handed back by a
//! guard for the same reason one rung further out: a run whose slot is never
//! cleared makes every later `runClean` answer `alreadyRunning` for the rest of
//! the session.

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cleaner_core::accel::{self, Preference};
use cleaner_core::detect::{DetBox, Detection, Detector, Letterbox, Segmentation};
use cleaner_core::rt_regions::FullRegions;
use cleaner_core::sam_ts;
use cleaner_core::text_groups::{
    self, EvidenceBlob, EvidenceModel, Execution, Grouping, ModelUse, Seam, SpatialInput,
};
use cleaner_core::engines::{fill, lama};
use cleaner_core::fit::{self, EdgeMap, Route};
use cleaner_core::gate::{Hayai, OutsideText, ScriptGate, Verdict};
use cleaner_core::image::{Raster, decode};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::memory;
use cleaner_core::patch::{Engine, Patch, Provenance};
use cleaner_core::project::{
    DetectedRegion, DetectionFit, Job, LoadedDetection, PatchRecord, PendingUntouched, Project, RegionUntouched,
    StripMode, buffers,
};
use cleaner_core::quality;
use cleaner_core::registry;
use cleaner_core::residency::{self, Resident};
use cleaner_core::strip::{
    self, DetectedInSegment, EngineContext, GlobalBox, Joins, LONGSTRIP_ASPECT, Segment, Split,
    SplitKind, Strip, Survey, merge_global,
};

use crate::events::{self, Event};
use crate::library::{self, ApiChapter, ApiPage, ApiProject, Library, LibraryError};

/* ------------------------------------------------------------------ */
/* The ladder ceiling                                                  */
/* ------------------------------------------------------------------ */

/// The rungs in order, so "no higher than" is a comparison rather than a match
/// arm somebody has to keep in the right sequence.
pub(crate) fn rung(engine: Engine) -> u8 {
    match engine {
        Engine::Fill => 0,
        // 1 is vacant too. It was Denoise fill's, removed once pages were
        // denoised whole, and left empty for the reason given below.
        Engine::Lama => 2,
        // 3 is vacant. It was MI-GAN's, and the removal left the number where
        // it was rather than sliding rung 3a and rung 4 down into it: every
        // stored `engineCeiling` is a *name*, but every comparison in this file
        // is on these numbers, and renumbering would silently reorder a ceiling
        // somebody stored. A gap costs nothing; a shuffle costs correctness.
        Engine::Flux => 4,
        Engine::Cloud => 5,
        // **Not rungs, and sorted above every ceiling so that they cannot be
        // mistaken for one.** A brush stroke and a clone stroke record
        // themselves in the same `engine` field a rung does, and that is the
        // whole of the resemblance: there is nothing to escalate to, nothing
        // to escalate from, and no quality verdict to escalate on. Answering
        // `u8::MAX` makes every "may this run here?" comparison - the ceiling
        // fold, `reachable_automatically`, `ladder_from`'s range filter -
        // answer no by arithmetic rather than by an arm somebody has to
        // remember to add. `Engine::is_rung` is the predicate that says so in
        // words; this is the number that makes it operational.
        Engine::Paint | Engine::Clone => u8::MAX,
    }
}

/// The highest rung that runs on this machine.
///
/// The whole of "auto clean is local-only". `Engine::Cloud` sits above it and is therefore unreachable from a run,
/// by construction rather than by a check somebody could forget.
pub const HIGHEST_LOCAL: Engine = Engine::Flux;

/// The highest rung an **automatic** run may reach, whatever ceiling it is
/// given.
///
/// [`HIGHEST_LOCAL`] answers "which rungs run on this machine" and this answers
/// "which rungs a batch job may spend", and rung 3a is the one place the two
/// differ: it is local, it is not automatic. Same ruling the cloud rung carries,
/// and for the same reason - `runClean` has no confirmation protocol, and
/// a batch job must not silently spend ten to sixty seconds a region on a heavy
/// local model, nor spawn a multi-gigabyte child to do it.
///
/// LaMa is the boundary because it is now the top of the automatic ladder: MI-GAN
/// held this line while it existed and no longer does. Rung 3a (FLUX) and
/// rung 4 (cloud) are still above it, and still for the reason the paragraph
/// above gives.
///
/// It is a **constant compared against**, rather than a rung left out of a
/// list, because a list is a thing somebody adds to. [`ladder_from`] filters on
/// this and `rung_3a_is_not_reachable_from_an_automatic_run` in
/// [`tests`](super::run::tests) pins it across every start and every ceiling.
pub const HIGHEST_AUTOMATIC: Engine = Engine::Lama;

/// Whether an automatic run may reach a rung at all.
///
/// The two rungs above [`HIGHEST_AUTOMATIC`] are the two that need a
/// confirmation `runClean` does not have: rung 4 spends the user's money and
/// sends their scan off the machine, rung 3a spends their memory and their
/// afternoon. Neither is a judgement about quality.
pub fn reachable_automatically(engine: Engine) -> bool {
    rung(engine) <= rung(HIGHEST_AUTOMATIC)
}

/// A rung name to a rung. Accepts the seam's bare names and the catalogue's
/// `ladder.rung.*` keys, because `engineCeiling` is a stored setting and the
/// two spellings both appear in the interface.
pub(crate) fn parse_rung(name: &str) -> Option<Engine> {
    match name.strip_prefix("ladder.rung.").unwrap_or(name) {
        // A stored `"denoise"` ceiling means the fill: rung 1 is gone, and a
        // ceiling at it allowed no model, which is what rung 0 still says.
        "fill" | "denoise" => Some(Engine::Fill),
        "lama" => Some(Engine::Lama),
        // **A stored `"migan"` still means something, and it means LaMa.**
        // MI-GAN was rung 3 and is gone, but a ceiling is a *setting*, written
        // to disk on machines that ran the old build. Dropping the arm outright
        // would make those installs fall to the `_ => None` case below, and an
        // unparseable ceiling is ignored - so the one setting whose whole job
        // is to hold a rung back would have silently released it, up to
        // `HIGHEST_LOCAL`. Harmless for an automatic run, which
        // `HIGHEST_AUTOMATIC` caps at LaMa anyway, and a real lift for a region
        // edit, which reads the same setting. LaMa is where "no higher than the
        // strongest automatic local rung" now points.
        "migan" => Some(Engine::Lama),
        "flux" => Some(Engine::Flux),
        "cloud" => Some(Engine::Cloud),
        _ => None,
    }
}

/// The rung a run may not go above.
///
/// **Caps, never raises.** Introduced twice and caught twice, so it is written as a fold that can
/// only ever move the ceiling down: the stored setting caps it, the caller's
/// argument caps it again, and [`HIGHEST_LOCAL`] caps both. An unparseable name
/// is ignored rather than treated as "no ceiling", because the failure that
/// matters is a typo silently unlocking a rung.
pub fn effective_ceiling(requested: Option<&str>, stored: Option<&str>) -> Engine {
    let mut ceiling = HIGHEST_LOCAL;
    for candidate in [stored, requested].into_iter().flatten().filter_map(parse_rung) {
        if rung(candidate) < rung(ceiling) {
            ceiling = candidate;
        }
    }
    ceiling
}

/// The stored key naming which accelerator the user wants.
///
/// The vocabulary is the interface's, as [`crate::settings`] insists, and this
/// is where it is read rather than defined - the same shape `engineCeiling`
/// already has a few lines above. The values are
/// [`cleaner_core::accel::Accelerator::label_key`] without the `accel.` prefix,
/// plus `auto`, so a settings panel can build the list from
/// `list_accelerators` and store the `id` it was given back verbatim.
pub const ACCELERATOR_KEY: &str = "accelerator";

/// What the user asked their models to run on.
///
/// Three answers and no fourth: `auto` is [`Preference::Automatic`], `cpu` is
/// [`Preference::CpuOnly`] - absolute, and the setting
/// the golden tests are written against -
/// and any provider name is [`Preference::Force`].
///
/// **An unreadable value is `Automatic`, not an error.** A settings file
/// written by a newer build, or by hand, must not stop the application from
/// cleaning a page; and `Force` of a provider that is not on this machine is
/// already handled one layer down, where it is reported rather than guessed at
/// ([`cleaner_core::accel::choose_within`]).
pub fn preference_from(settings: &serde_json::Value) -> Preference {
    let Some(name) = settings.get(ACCELERATOR_KEY).and_then(|value| value.as_str()) else {
        return Preference::Automatic;
    };
    accelerator_preference(name)
}

/// Per-model choices inherit the global setting until explicitly overridden.
/// Invalid saved overrides fail the run rather than quietly changing devices.
pub fn model_preference_from(settings: &serde_json::Value, id: &str) -> Result<Preference, String> {
    let Some(value) = settings.get("modelAccelerators").and_then(|v| v.get(id)) else {
        return Ok(preference_from(settings));
    };
    let name = value.as_str().ok_or_else(|| format!("{id} backend must be a string"))?;
    let normalized = name.strip_prefix("accel.").unwrap_or(name);
    if matches!(normalized, "auto" | "automatic" | "cpu")
        || cleaner_core::accel::KNOWN.iter().any(|a| a.label_key() == format!("accel.{normalized}")) {
        Ok(accelerator_preference(normalized))
    } else {
        Err(format!("Unsupported backend {name} for {id}"))
    }
}

/// The text reader's provider. A choice made for it is honoured or refused as
/// every model's is; one it only inherits from the whole-app setting falls back
/// to Automatic when the reader cannot run there (WebGPU, CoreML: see
/// `accel::HAYAI`), so a whole-app WebGPU choice made for LaMa never stops a
/// run or silences the reader.
pub(crate) fn reader_preference(settings: &serde_json::Value, host: Option<cleaner_core::runtime::package::Platform>)
    -> Result<Preference, String> {
    let chosen = settings.get("modelAccelerators").and_then(|v| v.get("hayai")).is_some();
    let preference = model_preference_from(settings, "hayai")?;
    match preference {
        Preference::Force(accelerator) if !crate::models::supports_model("hayai", accelerator, host) => {
            if chosen { Err(format!("hayai does not support {accelerator:?} on this platform")) } else { Ok(Preference::Automatic) }
        }
        other => Ok(other),
    }
}

/// The same, from the bare name. Split out so the mapping is testable without a
/// settings document.
pub fn accelerator_preference(name: &str) -> Preference {
    let name = name.strip_prefix("accel.").unwrap_or(name);
    match name {
        "auto" | "automatic" | "" => Preference::Automatic,
        "cpu" => Preference::CpuOnly,
        other => cleaner_core::accel::KNOWN
            .into_iter()
            .find(|accelerator| accelerator.label_key() == format!("accel.{other}"))
            .map(Preference::Force)
            .unwrap_or(Preference::Automatic),
    }
}

fn requested_accel_key(preference: Preference) -> &'static str {
    match preference {
        Preference::Force(accelerator) => accelerator.label_key(),
        Preference::Automatic | Preference::CpuOnly => accel::Accelerator::Cpu.label_key(),
    }
}

/// The rung that will actually run for a route, under a ceiling.
///
/// `None` is a decline. Rung 0 is always available - it is the bottom of the
/// ladder and needs no model - but [`Route::Inpaint`] under a ceiling below
/// rung 2 still **declines** rather than falling to it. §5 sent the region to
/// the ladder because the paper around it is periodic, multimodal or
/// unfittable, and rung 0 over that is a hole in the screentone. §6's metric
/// would not catch it either - a flat plate has no strokes to fail the edge
/// test, and its level sits inside a halftone's own p5..p95 - so the downgrade
/// would be a bad fill made silently. Leaving text is recoverable; a bad fill
/// is not.
///
/// The escalation above this is [`clean_region`]'s: this names where
/// a region **starts**, not where it ends.
pub(crate) fn engine_for(route: Route, ceiling: Engine) -> Option<Engine> {
    match route {
        Route::Fill => Some(Engine::Fill),
        Route::Inpaint => (rung(Engine::Lama) <= rung(ceiling)).then_some(Engine::Lama),
    }
}

/* ------------------------------------------------------------------ */
/* Which engine a kind of text starts on                               */
/* ------------------------------------------------------------------ */

/// What the user asked for, for one kind of text: **the rung it starts on**.
///
/// It used to be two answers - *fill* and *redraw* - each standing for a family
/// of rungs, on the reasoning that a translator should never have to read the
/// ladder's own words. The user ruled the other way: an engine picker exists so that
/// somebody can move between models, and a word that covers two of them makes
/// that impossible. So the tool window's two rows now name a rung each, and
/// this is that rung.
///
/// Rung 3a is deliberately not a variant. [`HIGHEST_AUTOMATIC`] caps every
/// automatic run at LaMa, so a `Flux` pick could only ever be a control that
/// did nothing; the tool window does not offer it and [`parse_pick`] does not
/// read it.
///
/// **Still a start and never a ceiling.** [`clean_region`] escalates past a
/// declined rung exactly as it did when this was two words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnginePick {
    /// Rung 0 - paint the hole from the paper around it.
    Fill,
    /// Rung 0 - paint the selected solid colour.
    Solid,
    /// Rung 2 - the inpainter.
    Lama,
}

impl EnginePick {
    /// The rung this pick starts a region on.
    fn engine(self) -> Engine {
        match self {
            EnginePick::Fill | EnginePick::Solid => Engine::Fill,
            EnginePick::Lama => Engine::Lama,
        }
    }
}

/// The two picks a run carries: one for text inside a speech balloon, one for
/// text outside one.
///
/// The defaults are the two kinds of text's own answers. A balloon is flat
/// white or flat black paper and the fill matches it exactly for no
/// model at all; text over art has to have the art put back, and only the
/// inpainter can do that. The run used to decide both from the fit alone, so a
/// bubble whose fit would not settle went to a 510 MB session and a second a
/// region to redraw paper a fill would have got right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picks {
    pub bubble: EnginePick,
    pub outside: EnginePick,
}

impl Default for Picks {
    fn default() -> Self {
        Picks { bubble: EnginePick::Fill, outside: EnginePick::Lama }
    }
}

impl Picks {
    /// The seam's two fields. An absent or unreadable name is **this type's own
    /// default**, not "no preference": the interface always sends both, so an
    /// unparseable one is a typo somewhere and the defaults are what the
    /// window shows.
    pub fn from_args(bubble: Option<&str>, outside: Option<&str>) -> Picks {
        let fallback = Picks::default();
        Picks {
            bubble: parse_pick(bubble).unwrap_or(fallback.bubble),
            outside: parse_pick(outside).unwrap_or(fallback.outside),
        }
    }

    /// Which pick a region answers to. The run's one in/out answer
    /// ([`cleaner_core::balloon::in_bubble`]: the balloon detector, or the
    /// paper around the lettering where the detector said *outside*), computed
    /// once per region and shared with the gate and the stored detection.
    fn for_region(&self, in_balloon: bool) -> EnginePick {
        if in_balloon { self.bubble } else { self.outside }
    }
}

/// A cloud clean's picks for the stored detections it plans, per side of a
/// balloon, and the ceiling they start under ([`repick_detections`]). A side
/// left `None` keeps each region's saved pick: the region menu sends only a
/// flat-colour bubble pick, and an outside region it cleans keeps its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Repick {
    pub bubble: Option<EnginePick>,
    pub outside: Option<EnginePick>,
    pub ceiling: Engine,
}

impl Repick {
    /// The seam's two names, each read as [`Picks::from_args`] reads it when
    /// it is sent, `None` when it is not. `None` when neither is sent.
    pub(crate) fn from_args(bubble: Option<&str>, outside: Option<&str>, ceiling: Engine) -> Option<Repick> {
        if bubble.is_none() && outside.is_none() { return None; }
        let fallback = Picks::default();
        Some(Repick {
            bubble: bubble.map(|name| parse_pick(Some(name)).unwrap_or(fallback.bubble)),
            outside: outside.map(|name| parse_pick(Some(name)).unwrap_or(fallback.outside)),
            ceiling,
        })
    }
}

fn pick_name(pick: EnginePick) -> &'static str {
    match pick {
        EnginePick::Fill => "fill",
        EnginePick::Solid => "solid",
        EnginePick::Lama => "lama",
    }
}

/// The source-language and optional reader choices captured when a run starts.
/// A CJK script identifier cannot reliably distinguish Japanese kanji from
/// Chinese Han; Han is therefore eligible when either language is selected.
/// Hangul and kana selections remain independently enforceable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunSelection {
    ja: bool,
    zh: bool,
    ko: bool,
    /// manga-ocr's rescue: the switch, and only while Japanese is cleaned.
    ocr_rescue: bool,
    /// The same switch as asked, for the Hayai reader ([`HAYAI_MODELS`]),
    /// which reads every language and runs under either text policy.
    reader: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DetectorModels { ctd: bool, rt_small: bool, rt_full: bool, sam: bool }

impl Default for DetectorModels {
    fn default() -> Self { Self { ctd: true, rt_small: true, rt_full: false, sam: false } }
}

impl DetectorModels {
    fn from_args(value: Option<&serde_json::Value>) -> Result<Self, String> {
        let Some(value) = value else { return Ok(Self::default()) };
        let ids = value.as_array().ok_or("detectorModels must be an array")?;
        let mut models = Self { ctd: false, rt_small: false, rt_full: false, sam: false };
        for id in ids {
            match id.as_str() {
                Some("ctd") => models.ctd = true,
                Some("rtSmall") => models.rt_small = true,
                Some("rtFull") => models.rt_full = true,
                Some("samTs") => models.sam = true,
                _ => return Err("Unsupported detection model".into()),
            }
        }
        if !models.ctd && !models.rt_small && !models.rt_full && !models.sam {
            return Err("Choose at least one detection model".into());
        }
        if models.rt_small && models.rt_full { return Err("Choose one Ogkalu detector profile, Full or Small".into()); }
        Ok(models)
    }

    fn required(self, all_text: bool) -> Vec<&'static str> {
        let mut files = Vec::new();
        if self.ctd { files.push(DETECTOR); }
        if self.rt_small { files.push(BALLOONS); }
        if !all_text { files.extend([GATE_MODEL, GATE_LABELS]); }
        files
    }
}

impl Default for RunSelection {
    fn default() -> Self {
        Self { ja: true, zh: true, ko: true, ocr_rescue: false, reader: false }
    }
}

impl RunSelection {
    pub(crate) fn from_args(detection: Option<&serde_json::Value>, ocr_rescue: Option<bool>) -> Result<Self, String> {
        let mut result = Self::default();
        if let Some(detection) = detection {
            let choices = detection.as_object().ok_or("detection must be a language-to-model map")?;
            for (language, selected) in choices {
                let enabled = match selected {
                    serde_json::Value::Null => false,
                    serde_json::Value::String(id) if id == "ctd-rtdetr" => true,
                    serde_json::Value::String(id) if id == "ctd-rtdetr-ocr" && language == "ja" => {
                        result.ocr_rescue = true;
                        true
                    }
                    _ => return Err(format!("unsupported detector choice for {language}")),
                };
                match language.as_str() {
                    "ja" => result.ja = enabled,
                    "zh" => result.zh = enabled,
                    "ko" => result.ko = enabled,
                    _ => return Err(format!("unsupported source language {language}")),
                }
            }
        }
        if let Some(enabled) = ocr_rescue { result.ocr_rescue = enabled; }
        result.reader = result.ocr_rescue;
        result.ocr_rescue &= result.ja;
        Ok(result)
    }

    /// The All text policy's selection: every language, no script gate, and
    /// the reader when the switch asks for it.
    pub(crate) fn all_text(reader: Option<bool>) -> Self {
        Self { reader: reader.unwrap_or(false), ..Self::default() }
    }

    fn any(self) -> bool { self.ja || self.zh || self.ko }

    fn empty_notice(self) -> Option<&'static str> {
        (!self.any()).then_some("notice.run.allLanguagesSkipped")
    }

    fn allows(self, verdict: &Verdict) -> bool {
        match verdict {
            Verdict::Clean { script } => {
                let script = script.strip_suffix("-dn").unwrap_or(script);
                match script {
                    "Japanese" | "Japanese_vert" | "Japanese_ocr" => self.ja,
                    "HanS" | "HanS_vert" | "HanT" | "HanT_vert" | "Han_ocr" => self.ja || self.zh,
                    "Hangul" | "Hangul_vert" | "Hangul_ocr" => self.ko,
                    _ => false,
                }
            }
            // Outside-bubble opt-in intentionally bypasses script reading.
            // A partial language selection cannot tell whether this region
            // belongs to a skipped language, so hold it for review.
            Verdict::OptedIn => self.ja && self.zh && self.ko,
            _ => false,
        }
    }
}

/// A pick's name to a pick.
///
/// The seam's names are the ladder's own rung names, bare or as a
/// `ladder.rung.*` catalogue key. `redraw` and `inpaint` are the two words the
/// rows used to send and are still read as rung 2: a preference stored before
/// the rename is not a typo, and silently
/// falling back to the default would move a user's run without telling them.
///
/// `flux` and `cloud` are **not** picks. Neither is reachable from an automatic
/// run ([`HIGHEST_AUTOMATIC`]), so reading either one as a start would be a
/// promise the ceiling immediately takes back.
pub(crate) fn parse_pick(name: Option<&str>) -> Option<EnginePick> {
    let name = name?;
    match name.strip_prefix("ladder.rung.").unwrap_or(name) {
        "fill" => Some(EnginePick::Fill),
        "solid" => Some(EnginePick::Solid),
        // Denoise fill is gone; a stored or sent `denoise` starts on the fill.
        "denoise" => Some(EnginePick::Fill),
        "lama" | "redraw" | "inpaint" => Some(EnginePick::Lama),
        _ => None,
    }
}

/// Parse a hex color string (e.g. "#ffffff" or "ffffff") to RGB bytes.
pub(crate) fn parse_color_hex(hex: Option<&str>) -> Option<[u8; 3]> {
    let text = hex?.trim();
    let s = text.strip_prefix('#').unwrap_or(text);
    if s.len() != 6 || !s.chars().all(|character| character.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

/// The rung a region **starts** on: the route's own answer, moved by the user's
/// pick for this kind of text.
///
/// **A pick is a starting point and never a ceiling.** It cannot reach past
/// `engineCeiling` - a `Lama` pick under a ceiling below rung 2 keeps the routed
/// rung - and it cannot remove a rung from [`ladder_from`], so a `Fill` over
/// screentone is tried, declined by [`quality::assess`], and escalated to the
/// inpainter exactly as an unpicked run would be. That is the whole reason the
/// pick moves the start rather than the ceiling: the cheap answer is tried
/// first and the expensive one is still there when it is wrong.
///
/// **The rung the row names is the rung that runs**, in both directions, which
/// is what was asked for. The one thing that still overrides it is the
/// ceiling.
///
/// The decline that [`engine_for`] returns is left exactly where it was. A
/// `Route::Inpaint` under a ceiling that cannot inpaint still declines rather
/// than being talked down to rung 0 by a pick - the routing argument is about the
/// paper under the region and a preference does not change it.
pub(crate) fn start_rung(route: Route, ceiling: Engine, pick: Option<EnginePick>) -> Option<Engine> {
    let routed = engine_for(route, ceiling)?;
    let Some(pick) = pick else { return Some(routed) };
    let preferred = pick.engine();
    Some(if rung(preferred) <= rung(ceiling) { preferred } else { routed })
}

/// The rungs a region may be tried on, in the order they are tried.
///
/// A decline is the state where "**no
/// engine's output** passes the uniform quality metric", which is a loop and not
/// a single call - `quality.rs`'s own report says so in as many words: "the
/// try-escalate-then-decline loop is the caller's". This is that loop's list.
///
/// Three filters, and each one is a rule from somewhere else:
///
/// - **At or above the routed rung.** The routing table is a floor, not a suggestion.
///   A region the fit could not settle does not get a *simpler* engine as a
///   second chance.
/// - **At or below the ceiling.** `engineCeiling` caps and never raises, and
///   it caps escalation for the same reason it caps routing.
/// - **The rung can carry this source.** The engines answer that themselves, so
///   an escalation cannot name a rung that would refuse the page,
///   one level up from [`fit::route_for`], which applies the same test when it
///   names the starting rung.
///
/// **Rung 2 is the top of this list**, and there is nothing above it a region
/// can escalate *to*. Rung 3 used to sit here - MI-GAN, the optional preview
/// tier, reached only when LaMa declined a region - and the user removed it on
/// the ground that a rung which only ever ran after a stronger model had
/// already refused the region was buying nothing. So the escalation this loop
/// walks now ends at the inpainter:
/// a region LaMa declines is declined, listed in review with its reason, and
/// left exactly as it was, which is §6's own answer for the end of the ladder.
///
/// **Rung 3a's FLUX sidecar is never reachable from an automatic run at all**,
/// and that is now a filter rather than an absence. It used to be true because
/// the array below did not mention it, which is a guarantee that lasts until
/// somebody adds a rung to a list; it is true now because
/// [`reachable_automatically`] says so and
/// `rung_3a_is_not_reachable_from_an_automatic_run` checks every start against
/// every ceiling. The rung exists - `cleaner_core::engines::flux` and
/// `cleaner_core::sidecar` - and it is reached per region, deliberately, from
/// Content-aware fill.
fn ladder_from(start: Engine, ceiling: Engine, page: &Raster) -> Vec<Engine> {
    [Engine::Fill, Engine::Lama, Engine::Flux, Engine::Cloud]
        .into_iter()
        .filter(|engine| reachable_automatically(*engine))
        .filter(|engine| rung(*engine) >= rung(start) && rung(*engine) <= rung(ceiling))
        .filter(|engine| match engine {
            // Rung 0 is arithmetic over the page's own samples and serves every
            // mode and depth this application reads.
            Engine::Fill => true,
            // The model rung answers for itself - a source it cannot carry is
            // a source it drops off the ladder rather than one it faults on.
            Engine::Lama => lama::applies(page),
            // Already gone, by the filter above. Written out rather than left
            // to a wildcard so that this match cannot silently start answering
            // for a rung it has no business answering for.
            Engine::Flux | Engine::Cloud => false,
            // The two hand tools are not on the ladder at all: `rung` sorts
            // them above every ceiling, so the range filter has already dropped
            // them and this arm is unreachable. Written out for the same reason
            // as the line above it.
            Engine::Paint | Engine::Clone => false,
        })
        .collect()
}

/* ------------------------------------------------------------------ */
/* What a page produced                                                */
/* ------------------------------------------------------------------ */

/// One region's result, in the three shapes the manifest has rows for.
pub enum RegionOutcome {
    /// A patch, and the `review.reason.*` key it is flagged under, if any.
    Cleaned(Box<Patch>, Option<String>),
    /// Deliberately left alone: gate-skipped, or declined. `reason` is an i18n
    /// key - no English is written to disk either.
    Untouched { bbox: Rect, reason: String },
    /// Lettering grouping held for the user's choice rather than cleaned
    /// (`reason` is its grouping key), with whether it sits inside a balloon.
    /// Stored as an untouched row, and counted as neither a decline nor a
    /// gate skip: nothing was refused. `lettering` is its glyphs in page
    /// pixels and the proxy scale they were found at, stored beside the row
    /// so a clean the user asks for writes through them and not the box
    /// (`None` for a weak text box, which has none).
    Candidate { bbox: Rect, reason: String, inside_bubble: bool, lettering: Option<(Mask, f32)> },
    /// Declined by the ladder in a one-pass clean: left exactly as it was and
    /// counted as a decline, like `Untouched`, but stored with the lettering
    /// the fit started from, in page pixels at its proxy scale, so a clean the
    /// user asks for writes through it. [`Job::decline_detection`] keeps the
    /// same for a stored detection. The decline is the metric's verdict on an
    /// automatic pick; a model the user names is run, not scored against
    /// (`region.rs`).
    Declined { bbox: Rect, reason: String, lettering: Option<(Mask, f32)> },
    /// Found, fitted and gated, and not cleaned: what Detect stops at. The
    /// masks are in page pixels. The record's `id` is empty, because the id is
    /// the manifest's to mint ([`Job::try_next_detection_id`]) and a pipeline has
    /// no manifest.
    Detected(Box<LoadedDetection>),
}

/// What a run does with each page it walks (`docs/detect-clean.md` §3).
///
/// A property of the run, captured with it and restored by a resume, like the
/// picks: a Detect run resumed as Auto would clean pages the user only asked
/// to have looked at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunMode {
    /// A page with stored detections is cleaned from them; any other page is
    /// detected and cleaned in one pass, as every run did before the split.
    #[default]
    Auto,
    /// Find, fit and store. Nothing is cleaned and no page is marked examined.
    Detect,
    /// Clean the stored detections. No detector opens.
    Clean,
}

impl RunMode {
    /// The seam's word. Strict: an unknown word is a caller that disagrees
    /// with this build about what it asked for, and running it as `auto`
    /// would clean pages a `detect` caller wanted left alone.
    pub fn from_arg(name: Option<&str>) -> Result<RunMode, String> {
        match name {
            None | Some("auto") => Ok(RunMode::Auto),
            Some("detect") => Ok(RunMode::Detect),
            Some("clean") => Ok(RunMode::Clean),
            Some(other) => Err(format!("Unsupported run mode {other}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RunMode::Auto => "auto",
            RunMode::Detect => "detect",
            RunMode::Clean => "clean",
        }
    }
}

/// The one area of a page a Detect is held to (`docs/detect-clean.md` §3), in
/// percent of the page as it is shown: what `ApiRegion.bbox` is.
///
/// Such a run looks again at text the page's Detect missed, or whose mask was
/// deleted. The models read a window around the area at the page's own
/// resolution ([`DetectArea::window`]), and what they find in the area is
/// added to what the page holds ([`Job::add_area_detection_results`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectArea {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl DetectArea {
    /// The side the models' window is grown to, in page pixels: the detectors'
    /// input side, and one cloud tile. A whole page is scaled down to that
    /// side, which is how small lettering is missed; an area is not.
    const WINDOW_SIDE: u32 = cleaner_core::cloud_analysis_wire::MAX_TILE_SIDE;

    /// The seam's `{x, y, w, h}`. Strict, like the mode: an area that is not
    /// on the page would run as a Detect of nothing.
    pub(crate) fn from_arg(value: Option<&serde_json::Value>) -> Result<Option<DetectArea>, String> {
        let Some(value) = value.filter(|value| !value.is_null()) else { return Ok(None) };
        let side = |key: &str| value.get(key).and_then(|v| v.as_f64()).filter(|v| v.is_finite());
        let (Some(x), Some(y), Some(w), Some(h)) = (side("x"), side("y"), side("w"), side("h")) else {
            return Err("detect_area_invalid".to_owned());
        };
        if x < 0.0 || y < 0.0 || w <= 0.0 || h <= 0.0 || x + w > 100.0 + 1e-6 || y + h > 100.0 + 1e-6 {
            return Err("detect_area_invalid".to_owned());
        }
        Ok(Some(DetectArea { x, y, w, h }))
    }

    fn to_value(self) -> serde_json::Value {
        serde_json::json!({ "x": self.x, "y": self.y, "w": self.w, "h": self.h })
    }

    /// The area in page pixels, at least one pixel and inside the page.
    fn on_page(self, width: u32, height: u32) -> Rect {
        let along = |start: f64, len: f64, extent: u32| {
            let from = ((start / 100.0) * f64::from(extent)).floor().clamp(0.0, f64::from(extent.saturating_sub(1)));
            let to = (((start + len) / 100.0) * f64::from(extent)).ceil().clamp(from + 1.0, f64::from(extent.max(1)));
            (from as i64, (to - from) as u32)
        };
        let (x, w) = along(self.x, self.w, width);
        let (y, h) = along(self.y, self.h, height);
        Rect::new(x, y, w, h)
    }

    /// What the models read for `area`: the area grown about its middle to
    /// [`DetectArea::WINDOW_SIDE`] on each axis, moved back inside the page.
    fn window(area: Rect, width: u32, height: u32) -> Rect {
        let along = |start: i64, len: u32, extent: u32| {
            let side = len.max(Self::WINDOW_SIDE).min(extent);
            let from = (start - i64::from(side - len.min(side)) / 2).clamp(0, i64::from(extent - side));
            (from, side)
        };
        let (x, w) = along(area.x, area.w, width);
        let (y, h) = along(area.y, area.h, height);
        Rect::new(x, y, w, h)
    }
}

/// What two rectangles share, empty when they share nothing.
fn shared(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    Rect::new(x, y, (a.right().min(b.right()) - x).max(0) as u32, (a.bottom().min(b.bottom()) - y).max(0) as u32)
}

/// What cleaning a page's stored detections produced, each result paired with
/// the id of the detection it replaces.
#[derive(Default)]
pub struct DetectionsOutcome {
    pub regions: Vec<(String, RegionOutcome)>,
    /// As [`PageOutcome::pressure`].
    pub pressure: Vec<&'static str>,
}

#[derive(Default)]
pub struct PageOutcome {
    pub regions: Vec<RegionOutcome>,
    /// Detector boxes for one-pass patches, keyed by the patch id. They are
    /// used for the same covered-text decision Detect applies before storing.
    pub detected_boxes: std::collections::HashMap<String, Rect>,
    /// The text-group evidence files one-pass patches name in their
    /// provenance (`params_snapshot.group.evidence`), each once. Written
    /// before the manifest row that names them, as Detect writes its own.
    pub evidence: Vec<EvidenceBlob>,
    /// Steps down the pressure ladder taken
    /// while this page was cleaned, as i18n keys.
    ///
    /// Here rather than emitted where it happens, because the pipeline has no
    /// event sink and should not grow one: the scheduler owns the stream, and
    /// the pipeline reporting through its return value is the same shape a
    /// region outcome already takes. A run whose ladder fired and said nothing
    /// would be a run that silently got slower.
    pub pressure: Vec<&'static str>,
}

/* ------------------------------------------------------------------ */
/* Where the page sits in the strip                                    */
/* ------------------------------------------------------------------ */

/// What the geometry knows about the page a [`Cleaner`] is about to clean.
///
/// Rules 1 to 3's geometry is built
/// and has no caller; this is how the caller reaches it. Everything here comes
/// from one [`strip::survey`] per job, taken before the walk starts, and it is
/// borrowed rather than owned because a 200-page chapter has exactly one of it.
pub struct PageContext<'a> {
    pub strip: &'a Strip,
    /// Rule 1's third state matters: an **unchecked** join is not verified, so
    /// reads do not cross it, and is not an anomaly, so nothing is reported.
    pub joins: &'a Joins,
    /// Every segment of the whole strip, because ownership
    /// ([`merge_global`]) is decided against all of them and not against this
    /// page's.
    pub segments: &'a [Segment],
    /// This page's position in `strip.pages()`, which is also its position in
    /// the manifest's `strip.order`.
    pub placement: usize,
    /// Resolved sources in strip order. Adjacent pages are decoded only when a
    /// verified join makes a bounded detection/engine window intersect them.
    pub sources: &'a [Option<PathBuf>],
}

impl PageContext<'_> {
    /// The page's own origin in strip coordinates.
    fn origin(&self) -> (i64, i64) {
        self.strip
            .pages()
            .get(self.placement)
            .map(|p| (p.x_offset, p.y_offset))
            .unwrap_or((0, 0))
    }

    /// The segments that lie inside this page. Segments are cut at page
    /// boundaries ([`strip::survey`]), so this is the whole of the page's
    /// detection work and never a fraction of a segment.
    pub fn page_segments(&self) -> Vec<Segment> {
        let Some(page) = self.strip.pages().get(self.placement) else { return Vec::new() };
        let (top, bottom) = (page.y_offset, page.rect().bottom());
        self.segments
            .iter()
            .copied()
            .filter(|s| (s.start as i64) >= top && (s.start as i64) < bottom)
            .collect()
    }

    /// The rows every segment boundary sits on, as rule 3 step 1's split lines.
    ///
    /// Step 1 unions two boxes whose facing edges lie within 8 px of the *same*
    /// split, which is how the two halves of a bubble a cut went through become
    /// one region. The lines that matter for that are the ones detection was
    /// actually cut at, which is every segment start - rule 2's plan and the
    /// page boundaries together.
    fn cuts(&self) -> Vec<Split> {
        self.segments
            .iter()
            .skip(1)
            .map(|s| Split { y: s.start, kind: SplitKind::Join })
            .collect()
    }

    /// A rectangle in a segment crop's coordinates, in strip coordinates.
    /// `top` is where the crop begins in the page.
    #[cfg(test)]
    fn to_strip(&self, rect: Rect, top: u32) -> Rect {
        let (x, y) = self.origin();
        Rect::new(rect.x + x, rect.y + y + top as i64, rect.w, rect.h)
    }

    fn read_window(&self, window: strip::DecodeWindow, current: &Raster) -> Result<strip::WindowRaster, String> {
        strip::read_window_borrowing(self.strip, window, |position| {
            if position == self.placement {
                return Ok(std::borrow::Cow::Borrowed(current));
            }
            let path = self.sources.get(position).and_then(Option::as_ref)
                .ok_or_else(|| format!("no source for strip position {position}"))?;
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            decode(&bytes).map(|p| cleaner_core::image::orientation::from_bytes(&bytes).raster(&p)).map(std::borrow::Cow::Owned).map_err(|e| e.to_string())
        })
    }

    /// The reverse, clamped to the crop: a decode window may reach onto another
    /// page through a verified join, and this build's reader holds one page.
    #[cfg(test)]
    fn to_crop(&self, rect: Rect, top: u32, crop: &Raster) -> Rect {
        let (x, y) = self.origin();
        let x0 = (rect.x - x).max(0);
        let y0 = (rect.y - y - top as i64).max(0);
        let x1 = (rect.right() - x).min(crop.width as i64);
        let y1 = (rect.bottom() - y - top as i64).min(crop.height as i64);
        Rect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
    }
}

/// The rows and columns the models read for one segment: the segment's whole
/// band, or the part of it inside `within` (strip coordinates) for a Detect
/// held to an area.
fn detection_window(context: &PageContext<'_>, segment: Segment, within: Option<Rect>) -> strip::DecodeWindow {
    let band = Rect::new(
        0,
        segment.start as i64,
        context.strip.width(),
        segment.detect_end.saturating_sub(segment.start),
    );
    let requested = within.map_or(band, |window| shared(band, window));
    let rect = context.strip.clamp(requested, context.joins);
    strip::DecodeWindow { rect, requested, pad: strip::EdgePad::None }
}

fn anchored_engine_window(
    context: &PageContext<'_>,
    boxr: Rect,
    scale: f32,
    engine: EngineContext,
) -> strip::DecodeWindow {
    let mut window = strip::decode_window(context.strip, context.joins, boxr, scale, engine);
    if window.rect.x <= boxr.x && window.rect.y <= boxr.y
        && window.rect.right() >= boxr.right() && window.rect.bottom() >= boxr.bottom() {
        return window;
    }
    let Some(page) = context.strip.pages().get(context.placement) else { return window };
    let page = page.rect();
    let x = window.requested.x.max(page.x);
    let y = window.requested.y.max(page.y);
    let right = window.requested.right().min(page.right());
    let bottom = window.requested.bottom().min(page.bottom());
    window.rect = Rect::new(x, y, (right - x).max(0) as u32, (bottom - y).max(0) as u32);
    window.pad = strip::EdgePad::Replicate;
    window
}

/// What turns one page of pixels into region outcomes.
///
/// See the module docs for why this is a trait. `page_id` is passed in because
/// region ids are page-scoped and the manifest keeps one record per id across
/// the whole chapter, and `context` because
/// rules 1 to 4 are all statements
/// about where the page sits in the strip rather than about the page.
pub trait Cleaner: Send {
    fn clean_page(
        &mut self,
        page_id: &str,
        bytes: &[u8],
        ceiling: Engine,
        context: &PageContext,
    ) -> Result<PageOutcome, String>;

    /// [`Cleaner::clean_page`] stopped after the fit: every region the gate
    /// would clean comes back as [`RegionOutcome::Detected`] rather than as a
    /// patch, and the rest as the untouched rows a one-pass run leaves.
    ///
    /// Defaulted to a refusal so a cleaner that has no detection of its own -
    /// the scheduler's test stub - fails the page it is asked for rather than
    /// cleaning it when only detection was asked for.
    fn detect_page(
        &mut self,
        _page_id: &str,
        _bytes: &[u8],
        _ceiling: Engine,
        _context: &PageContext,
    ) -> Result<PageOutcome, String> {
        Err("this cleaner cannot detect without cleaning".into())
    }

    /// Cloud Detect reads the displayed page and adds fresh records. Other
    /// pipelines retain their existing detection and resume behavior.
    fn cloud_detection(&self) -> bool { false }

    /// Whether this Detect is held to one area of its page ([`DetectArea`]):
    /// what it finds is added to the page's detections, and nothing the page
    /// holds is replaced.
    fn area_only(&self) -> bool { false }

    fn detect_current_page(
        &mut self, page_id: &str, bytes: &[u8], ceiling: Engine,
        context: &PageContext, _job: &Job,
    ) -> Result<PageOutcome, String> {
        self.detect_page(page_id, bytes, ceiling, context)
    }

    /// Clean a page's stored detections, each from its own stored mask, with
    /// the ladder a one-pass run uses. **Nothing is detected**: no detector,
    /// balloon detector or gate opens.
    fn clean_detections(
        &mut self,
        _page_id: &str,
        _bytes: &[u8],
        _ceiling: Engine,
        _context: &PageContext,
        _detections: &[LoadedDetection],
    ) -> Result<DetectionsOutcome, String> {
        Err("this cleaner cannot clean stored detections".into())
    }

    /// The model fault the last [`Cleaner::clean_page`] failed on, when it
    /// failed on a model rather than on the page.
    ///
    /// Asked for here rather than carried out through the error, because the
    /// error is a `String` and the two things a run has to say about a fault -
    /// which model gave up, and which provider it was running on - are
    /// catalogue keys that no string can be parsed back into. Widening the
    /// error type would reach the test stub and `spikes/clean-page` for a fact
    /// neither of them has; a defaulted accessor reaches only the
    /// implementation that has one.
    ///
    /// `None` after a failed page means the page failed for its own reasons -
    /// it could not be read, or it could not be decoded - and the run reports
    /// it with the count alone.
    fn last_fault(&self) -> Option<EngineFault> {
        None
    }

    /// Why the run must stop after the page that just failed, when it must.
    ///
    /// A page that fails is counted and the run moves on, as it always has.
    /// A cloud failure that withdraws the run's authority (the endpoint or
    /// profile changed, cloud engines were switched off, the endpoint refused
    /// the key) would fail every later page the same way, and must not keep
    /// knocking on the endpoint, so it ends the walk. `None` for every other
    /// failure and for every cleaner without cloud stages.
    fn stop_reason(&self) -> Option<String> {
        None
    }
}

/// What a page failed on when it failed on a **model** rather than on the page.
///
/// Windows is the machine this exists for. A Direct3D device can be reset by
/// the driver's watchdog in the middle of an inference - two seconds is the
/// default patience - and ONNX Runtime reports the reset as a device removed or
/// a device hung; the same session can also simply run out of VRAM. None of the
/// three is recoverable on the session it happened to, so the only honest thing
/// a run can say afterwards is what faulted and where it was running. Nothing
/// in this repository has ever been executed on Windows, which is the reason
/// the sentence is built from two keys rather than from a message: the keys are
/// facts this process holds, and the message would be a guess.
#[derive(Clone, Debug)]
pub struct EngineFault {
    /// [`cleaner_core::accel::ModelProfile::label_key`] - a `models.kind.*`
    /// key, the same one the loaded-models tab names this model under.
    pub model_key: &'static str,
    /// [`cleaner_core::accel::Accelerator::label_key`] - an `accel.*` key.
    pub accel_key: &'static str,
    /// ONNX Runtime's own words for it. **Never shown to the user**: it is not
    /// a catalogue key, so no seam can translate it, and the sentence the user
    /// reads is built from the two keys above instead. It is carried so the
    /// detail is not lost - [`walk`] writes it to stderr beside the notice,
    /// which is where this crate already puts diagnostic text a user cannot act
    /// on ([`open_gate`] does the same for a reader that will not open).
    pub detail: String,
}

/// The two things this file needs from an ONNX session after one of its **run**
/// calls has failed.
///
/// An open failure is a different thing and is already handled elsewhere:
/// [`cleaner_core::accel::open_session`] falls back to the CPU, and a session
/// that will not build at all leaves its rung unavailable and says so. This is
/// for the session that built, ran, and then died - the case where keeping it
/// is worse than never having had it, because once a Direct3D device is lost
/// every later run on that session fails too.
pub(crate) trait Faulted {
    /// [`cleaner_core::registry::Lease::poison`], through whichever engine
    /// holds the lease.
    ///
    /// From here on the row reads spent, and **that is the whole disposal
    /// mechanism**: [`OnDemand::release_if_spent`] and
    /// [`Rung2::release_if_spent`] drop the session at the next region
    /// boundary, [`residency::checkin`] refuses to park it, and
    /// [`residency::checkout`] refuses to hand it out. It is the same path a
    /// session the user closed from the loaded-models tab already takes, which
    /// is why a fault needs no machinery of its own.
    fn poison_session(&self);

    /// Where this session was running, as the key the interface names the
    /// accelerator under.
    fn accel_key(&self) -> &'static str;
}

impl Faulted for Detector {
    fn poison_session(&self) {
        Detector::poison(self);
    }

    fn accel_key(&self) -> &'static str {
        self.selection().accelerator.label_key()
    }
}

impl Faulted for cleaner_core::balloon::BalloonDetector {
    fn poison_session(&self) {
        cleaner_core::balloon::BalloonDetector::poison(self);
    }

    fn accel_key(&self) -> &'static str {
        self.selection().accelerator.label_key()
    }
}

impl Faulted for ScriptGate {
    fn poison_session(&self) {
        ScriptGate::poison(self);
    }

    fn accel_key(&self) -> &'static str {
        self.selection().accelerator.label_key()
    }
}

impl Faulted for lama::Inpainter {
    fn poison_session(&self) {
        lama::Inpainter::poison(self);
    }

    fn accel_key(&self) -> &'static str {
        self.selection().accelerator.label_key()
    }
}

/// One run call on a session, with everything that has to happen when it fails.
///
/// Three steps, and every one of them used to be missing. The session is
/// **poisoned**, so nothing later in this run - or in any later run, or in a
/// hand edit between two of them - is handed the dead one. The fault is
/// **recorded**, so the run loop can name the model and the provider in a
/// notice instead of leaving the user with a silent count. And the diagnostic
/// text is **passed on**, so the page still fails with something a developer
/// can read.
///
/// `slot` arrives as a borrow of the pipeline's field rather than through
/// `&mut self` because `session` is itself borrowed out of the pipeline: the
/// two are different fields, and taking them separately is what lets each call
/// site stay three lines.
fn ran<T, E: std::fmt::Display>(
    result: Result<T, E>,
    session: &dyn Faulted,
    profile: &accel::ModelProfile,
    slot: &mut Option<EngineFault>,
) -> Result<T, String> {
    result.map_err(|error| {
        session.poison_session();
        let detail = error.to_string();
        *slot = Some(EngineFault {
            model_key: profile.label_key,
            accel_key: session.accel_key(),
            detail: detail.clone(),
        });
        detail
    })
}

/* ------------------------------------------------------------------ */
/* The real pipeline                                                   */
/* ------------------------------------------------------------------ */

/// The model files a run opens, by the names `scripts/fetch-models.sh` writes.
pub(crate) const DETECTOR: &str = "comictextdetector.onnx";
pub(crate) const BALLOONS: &str = "comic-text-and-bubble-detector-detector-v4-s_int8.onnx";
pub(crate) const GATE_MODEL: &str = "image-script-identification-osd_lstm.onnx";
pub(crate) const GATE_LABELS: &str = "image-script-identification-osd_labels.json";

/// Rung 2's weights. The fifth model, and the one a run does not open until
/// something needs it - see [`Rung2`].
pub(crate) const INPAINTER: &str = "lama-manga.onnx";

/// The gate's rescue reader - `manga-ocr`, three files
/// ([`cleaner_core::gate::ocr`]), and **optional in a way none of the others
/// are**. It is not in [`REQUIRED_MODELS`], nothing falls back to it, and a
/// machine without it gates exactly as this application did before it existed:
/// a selected run attaches it when all three files are present and otherwise
/// opens the gate alone with a notice. 460 MB is too much to make a precondition of cleaning
/// a page for the 4% of regions it recovers.
pub(crate) const OCR_ENCODER: &str = "manga-ocr-encoder_model.onnx";
pub(crate) const OCR_DECODER: &str = "manga-ocr-decoder_model.onnx";
pub(crate) const OCR_VOCAB: &str = "manga-ocr-vocab.txt";

/// The three, in one place, so a caller asking "is the reader installed" asks
/// the same question the selected run answers.
pub(crate) const OCR_MODELS: [&str; 3] = [OCR_ENCODER, OCR_DECODER, OCR_VOCAB];

/// The multi-script reader ([`cleaner_core::gate::hayai`]), optional like
/// `manga-ocr`. When all three files are present a run uses it on every
/// region the gate would clean without reading a script (the text check) and
/// on every failed in-balloon verdict (the rescue), and `manga-ocr` is not
/// opened.
pub(crate) const HAYAI_VISION: &str = "hayai-ocr-vision.onnx";
pub(crate) const HAYAI_DECODER: &str = "hayai-ocr-decoder.onnx";
pub(crate) const HAYAI_TOKENIZER: &str = "hayai-ocr-tokenizer.json";
pub(crate) const HAYAI_MODELS: [&str; 3] = [HAYAI_VISION, HAYAI_DECODER, HAYAI_TOKENIZER];

fn hayai_installed(models: &Path) -> bool {
    HAYAI_MODELS.iter().all(|name| std::fs::metadata(models.join(name)).is_ok_and(|m| m.len() > 0))
}

fn open_hayai(models: &Path, preference: Preference) -> Result<Hayai, String> {
    Hayai::open(&models.join(HAYAI_VISION), &models.join(HAYAI_DECODER), &models.join(HAYAI_TOKENIZER), preference)
        .map_err(|e| e.to_string())
}

/// The model files a run requires to open a pipeline.
///
/// `DETECTOR`, `BALLOONS`, `GATE_MODEL`, and `GATE_LABELS` are required eagerly
/// by [`Pipeline::open`]. `INPAINTER` is opened lazily by rung 2 and falls back
/// to `Unavailable` if missing on disk, so it is not required at open time.
pub(crate) const REQUIRED_MODELS: [&str; 4] = [DETECTOR, BALLOONS, GATE_MODEL, GATE_LABELS];

/// Whether a candidate directory contains all files required to open a [`Pipeline`].
pub(crate) fn models_ready(dir: &Path) -> bool {
    REQUIRED_MODELS.iter().all(|name| dir.join(name).exists())
}

/// Why [`Pipeline::open`] said no.
///
/// One variant, because [`Pipeline::open`] has exactly one way to fail: the
/// files are not there. It used to answer a `String`, and [`start`] told the
/// missing-weights failure apart from every other one by reading the front and
/// the back of that string - which
/// made the *wording* of a message load-bearing, and needed a test to pin it so
/// that rephrasing the message could not silently turn a notice into a rejected
/// promise.
///
/// A type instead. The wording stays exactly what it was, because
/// the remedy is what the user reads, but
/// nothing branches on it any more. And when `open` grows a second failure -
/// a runtime that will not load, a provider that refuses - the match in
/// [`start`] stops compiling until someone decides whether that one is a notice
/// or a rejection, which is the decision the prefix was making by accident.
#[derive(Debug)]
pub enum OpenError {
    /// A file in [`REQUIRED_MODELS`] is not on disk. The remedy is Settings ›
    /// Models, and [`start`] turns this into `notice.run.modelsMissing`.
    MissingModel { path: PathBuf },
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::MissingModel { path } => {
                write!(f, "the model file {} is missing", path.display())
            }
        }
    }
}

impl std::error::Error for OpenError {}

/// For the callers that answer `Result<_, String>` - every Tauri command in
/// this crate does - so that `?` still reaches them without each one spelling
/// out `map_err(|e| e.to_string())`.
impl From<OpenError> for String {
    fn from(error: OpenError) -> String {
        error.to_string()
    }
}

/// Where the weights are, in the order they are looked for.
///
/// The same shape as [`cleaner_core::runtime::search_paths`] and for the same
/// reasons: an environment override so a developer can point at a checkout, the
/// per-user directory a first-launch download writes to,
/// then the installed copy beside the
/// executable and inside the macOS bundle's `Resources`. The working directory
/// is last, and it is what the spikes and `cargo test` use.
pub fn model_search_paths(app_data: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(explicit) = std::env::var("MANGA_CLEANER_MODELS") {
        if !explicit.is_empty() {
            paths.push(PathBuf::from(explicit));
        }
    }
    if let Some(dir) = app_data {
        paths.push(dir.join("models"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join("models"));
            paths.push(dir.join("../Resources/models"));
        }
    }
    paths.push(PathBuf::from("models"));
    paths
}

pub(crate) fn model_dir(app_data: Option<&Path>) -> Option<PathBuf> {
    model_search_paths(app_data).into_iter().find(|dir| models_ready(dir))
}

fn selected_model_dir(app_data: Option<&Path>, selection: DetectorModels, all_text: bool) -> Option<PathBuf> {
    let required = selection.required(all_text);
    model_search_paths(app_data).into_iter()
        .find(|dir| required.iter().all(|name| dir.join(name).exists()))
}

/// What the selected models answered for one segment crop: Ogkalu's boxes,
/// CTD's boxes and segmentation, and SAM-TS-L's mask.
type ModelAnswers = (Vec<cleaner_core::balloon::BalloonBox>, Detection, Option<Vec<u8>>);

/// What one segment's detection produced.
struct SegmentDetection {
    balloons: Vec<cleaner_core::balloon::BalloonBox>,
    detection: Detection,
    /// The text groups every later step cleans by
    /// ([`cleaner_core::text_groups`]), whichever models ran.
    grouping: Grouping,
    /// That grouping's evidence file: the pixel mask, every raw box, the
    /// component assignment and any ink estimate, written beside the manifest
    /// with the detections.
    evidence: EvidenceBlob,
}

/// One segment's detection, kept for as long as a region it saw is waiting on
/// a later segment: the crop and everything that region is judged, fitted and
/// recorded against.
struct SeenSegment {
    index: usize,
    crop: Raster,
    /// The crop's origin in strip coordinates.
    origin: (i64, i64),
    /// The strip origin of the page the grouping's coordinates are on.
    page_origin: (i64, i64),
    segmentation: Segmentation,
    balloons: Vec<cleaner_core::balloon::BalloonBox>,
    grouping: Grouping,
    evidence: EvidenceBlob,
}

impl SeenSegment {
    /// The grouping and its evidence file in the coordinates of the page at
    /// `page_origin`: the segment's own, or, for a region the next page owns
    /// and writes, rebased onto that page. Group ids stay those of the page
    /// the evidence was found on.
    fn evidence_on(&self, page_origin: (i64, i64)) -> Result<(std::borrow::Cow<'_, Grouping>, EvidenceBlob), String> {
        if page_origin == self.page_origin {
            return Ok((std::borrow::Cow::Borrowed(&self.grouping), self.evidence.clone()));
        }
        let file = text_groups::EvidenceFile::decode(&self.evidence.bytes)?;
        let mut grouping = file.grouping;
        grouping.origin = (self.origin.0 - page_origin.0, self.origin.1 - page_origin.1);
        let evidence = grouping.evidence(Some(&file.mask))?;
        Ok((std::borrow::Cow::Owned(grouping), evidence))
    }
}

/// A region one segment saw in its detection overlap that a later segment
/// owns. It is written from `seen` only if that owner does not find it.
struct Deferred {
    seen: Arc<SeenSegment>,
    region: cleaner_core::detect::Region,
    /// The region's rectangle in strip coordinates.
    rect: Rect,
    /// The same, as ownership evidence.
    found: DetectedInSegment,
    /// The segment that owns it, as of the last segment that ran.
    owner: usize,
}

/// A region's lettering, in strip coordinates, as a claim on candidates.
struct Claim {
    segment: usize,
    /// The region's rectangle.
    rect: Rect,
    lettering: Mask,
}

impl Claim {
    fn of(region: &cleaner_core::detect::Region, segment: usize, (dx, dy): (i64, i64)) -> Claim {
        let rect = Rect::new(region.masking.x + dx, region.masking.y + dy, region.masking.w, region.masking.h);
        let lettering = region.group.as_ref()
            .map_or_else(|| Mask::empty(Rect::new(rect.x, rect.y, 0, 0)), |held| translated(held.lettering.clone(), dx, dy));
        Claim { segment, rect, lettering }
    }
}

/// A candidate one segment listed, held until every region that could claim
/// its lettering is known. Its lettering is kept as its 8-connected islands
/// (the group's components), in strip coordinates.
struct HeldCandidate {
    segment: usize,
    /// The group's bounds (its anchor, for one with no lettering).
    rect: Rect,
    islands: Vec<Mask>,
    reason: &'static str,
    /// [`cleaner_core::balloon::in_bubble`] for the group, asked exactly as a
    /// cleaning region's is ([`candidate_in_bubble`]).
    inside_bubble: bool,
    /// The proxy scale of the segmentation that found it.
    scale: f32,
}

/// Whether a held candidate sits in a bubble: the region a cleaning group of
/// the same bounds would be, put to [`cleaner_core::balloon::in_bubble`] with
/// the same crop, segmentation and balloon boxes its segment's regions are.
/// Not the grouping's own balloon confinement (`group.bubble`), which only
/// knows the balloon detector's boxes and would disagree with the gate and
/// the engine pick wherever the paper rescued text.
fn candidate_in_bubble(
    grouping: &Grouping,
    group: &text_groups::TextGroup,
    crop: &Raster,
    segmentation: &cleaner_core::detect::Segmentation,
    balloons: &[cleaner_core::balloon::BalloonBox],
) -> bool {
    let region = grouping.region(group, 0);
    cleaner_core::balloon::in_bubble(crop, segmentation, region.text_bounds(), region.masking, balloons)
}

impl HeldCandidate {
    fn of(
        grouping: &Grouping, group: &text_groups::TextGroup, segment: usize, (dx, dy): (i64, i64), scale: f32,
        inside_bubble: bool,
    ) -> HeldCandidate {
        let lettering = grouping.lettering(group);
        let (lx, ly) = (lettering.bounds.x + dx, lettering.bounds.y + dy);
        let labeled = text_groups::label(lettering.bounds.w, lettering.bounds.h, &lettering.bits);
        let mut islands: Vec<Mask> = labeled.components.iter()
            .map(|(bounds, _)| Mask::empty(Rect::new(bounds.x + lx, bounds.y + ly, bounds.w, bounds.h)))
            .collect();
        let width = lettering.bounds.w as usize;
        for (at, &label) in labeled.labels.iter().enumerate() {
            if label != 0 {
                islands[label as usize - 1].set((at % width) as i64 + lx, (at / width) as i64 + ly, true);
            }
        }
        HeldCandidate {
            segment,
            rect: Rect::new(group.bounds.x + dx, group.bounds.y + dy, group.bounds.w, group.bounds.h),
            islands,
            reason: group.reasons.iter().min()
                .map_or(text_groups::ReviewReason::UnassignedComponent.key(), |reason| reason.key()),
            inside_bubble,
            scale,
        }
    }
}

/// A candidate already listed: its rectangle and the islands it listed, in
/// strip coordinates.
struct Listed {
    rect: Rect,
    islands: Vec<Mask>,
}

/// What a page's segments leave for the next page's, when that page is next
/// in the run: the last segment's boxes as ownership evidence, the regions
/// and candidates a later page owns, and the lettering claims and listed
/// candidates that reach onto it.
#[derive(Default)]
struct Carried {
    found: Vec<DetectedInSegment>,
    deferred: Vec<Deferred>,
    candidates: Vec<HeldCandidate>,
    claims: Vec<Claim>,
    listed: Vec<Listed>,
}

/// Everything a page's rows are written against, whichever segment's crop a
/// region was seen in, and the rows written so far.
struct PageEmission<'a, 'c> {
    page_id: &'a str,
    page: &'a Raster,
    context: &'a PageContext<'c>,
    /// The page's origin in strip coordinates.
    origin: (i64, i64),
    noise: f32,
    engine_context: EngineContext,
    ceiling: Engine,
    detect_only: bool,
    source_sha256: &'a str,
    provider: String,
    outcome: PageOutcome,
    /// The next region index on the page.
    index: usize,
}

/// Every session a run may need, and **not one of them opened until a page
/// asks for it**.
///
/// This used to open the detector, the balloon detector and the script gate in
/// [`Pipeline::open`] and hold all three for the length of the run. Both halves
/// of that were wrong for a run's memory:
///
/// - **Opening them eagerly** put three sessions on a run before it had decided
///   there was anything to do, and on the *only* code path that can fail for a
///   reason the user can act on - missing weights. So `open` now checks that
///   the files are there, which is the failure worth reporting early, and opens
///   nothing.
/// - **Holding them unconditionally** meant a session with nothing pending was
///   indistinguishable from one in use. [`OnDemand`] closes that: every model
///   carries a [`cleaner_core::registry`] row, and a row can be asked for back -
///   by the user, from the loaded-models tab, or by the idle grace - at the
///   region boundary this file already established as the safe point for
///   giving a session back.
pub struct Pipeline {
    detector: OnDemand<Detector>,
    balloons: OnDemand<cleaner_core::balloon::BalloonDetector>,
    gate: OnDemand<ScriptGate>,
    /// The Hayai reader, used only while `reads` is true.
    reader: OnDemand<Hayai>,
    /// Whether this run reads regions with Hayai: the run's selection asked
    /// for the reader, its files were installed when the selection was set,
    /// and it has not failed to open since.
    reads: bool,
    detection_models: DetectorModels,
    all_text: bool,
    app_data: Option<PathBuf>,
    full_rt: Option<FullRegions>,
    sam: Option<sam_ts::SamTsSession>,
    full_rt_preference: Preference,
    sam_preference: Preference,
    engine_version: &'static str,
    rung2: Rung2,
    /// The pressure ladder, latched for the life of the pipeline. See
    /// [`Pipeline::under_pressure`].
    ladder: memory::Ladder,
    /// Which rung each kind of text starts on. A property of the run and not of
    /// the page, so it is held here rather than passed through [`Cleaner`]: the
    /// trait's arguments are the ones that change page by page.
    ///
    /// `None` is **not** [`Picks::default`]: it is "nobody asked", and a
    /// pipeline nobody asked routes every region by the fit alone, which is
    /// what this did before the rows existed. `run_clean` always asks - the
    /// tool window always sends both - so the empty case is `spikes/clean-page`
    /// and the tests, where the fit's own answer is the thing under test.
    picks: Option<Picks>,
    bubble_color: Option<[u8; 3]>,
    /// Total page-pixel padding around the segmentation seed before storing
    /// or cleaning. Zero keeps the SAM mask without automatic fit growth.
    mask_padding: u32,
    /// What this run does with text the balloon question puts outside a
    /// balloon. [`cleaner_core::gate::OutsideText::Review`] unless the tool
    /// window's row said otherwise, and a property of the run for the same
    /// reason `picks` is.
    outside: OutsideText,
    /// A run-local copy of the language choices. Settings changes made while
    /// a chapter is running cannot change which regions that run may clean.
    selection: RunSelection,
    /// What the last page failed on, when it failed on a model. Cleared at the
    /// top of every [`Cleaner::clean_page`] so that it answers for the page the
    /// run loop is asking about and not for one three pages ago, and read back
    /// through [`Cleaner::last_fault`].
    fault: Option<EngineFault>,
    /// What the last page's segments left for the next page's
    /// ([`Carried`]), and which page that was.
    carried: Carried,
    previous_placement: Option<usize>,
    /// The cloud stages this run was granted, if any. See the module docs.
    /// Handed to `prefetch` when the run starts; a pipeline nobody started one
    /// for analyzes each page inline instead.
    remote: Option<crate::inference::run_analysis::RemoteDetection>,
    /// The run's cloud analysis, running ahead of its cleaning.
    prefetch: Option<crate::inference::run_analysis::Prefetch>,
    /// The current page's cloud answer, in page coordinates.
    remote_page: Option<crate::inference::run_analysis::RemotePage>,
    /// On a long strip, the later pages' cloud answers that the current
    /// page's detection windows reach into across a join, each with its
    /// origin relative to the current page's, in strip order. A window that
    /// crosses a join is read whole on this computer, so its cloud evidence
    /// has to be whole too, or a bubble cut by the join is found in halves.
    remote_below: Vec<((i64, i64), crate::inference::run_analysis::RemotePage)>,
    /// Answers taken ahead of the walk for [`Pipeline::remote_below`] on a
    /// run that analyzes each page inline: by placement, with the digest of
    /// the bytes they were analyzed from. The walk takes one when it reaches
    /// its page, so no page is sent twice.
    remote_ahead: Vec<(usize, String, Result<crate::inference::run_analysis::RemotePage, String>)>,
    /// Set when a cloud failure withdrew the run's authority; see
    /// [`Cleaner::stop_reason`].
    remote_stop: Option<String>,
    /// Hold a solid fill to the quality metric as well, for a cloud clean's
    /// local first pass ([`clean_fill_rung_first`]). Off for every run.
    gate_solid_fill: bool,
    /// The one area this run's Detect is held to, if it is. A property of the
    /// run, like `picks`.
    area: Option<DetectArea>,
    #[cfg(test)]
    test_vision: Option<TestVision>,
    /// How many later pages' answers [`Pipeline::gather_below`] has read.
    #[cfg(test)]
    answers_below: usize,
    /// A lettering mask standing in for SAM-TS-L under [`TestVision`], which
    /// sends the segment through the grouped path.
    #[cfg(test)]
    test_lettering: Option<fn(&Raster, usize) -> Vec<u8>>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct TestVision {
    detect: fn(&Raster, usize) -> cleaner_core::detect::Detection,
    balloons: fn(&Raster, usize) -> Vec<cleaner_core::balloon::BalloonBox>,
    judge: fn(&cleaner_core::detect::Region) -> Verdict,
}

/// A session borrowed the first time something needs it and handed back to
/// [`cleaner_core::residency`] the moment this run does not.
///
/// **This no longer owns the session, and that is the whole change.** It used
/// to: the detector belonged to the `Pipeline`, the `Pipeline` was a local of
/// the run's thread, and so the end of a run - or of a region edit, which built
/// its own bench - dropped every model the user's next click was about to want.
/// Now `get` asks the residency cache first and `Drop` puts the session back
/// there, so two consecutive runs, a page turn and a hand edit in between all
/// share one 94 MB session and one CoreML build.
///
/// The three models a page needs before it can route anything - the detector,
/// the balloon detector, the gate - differ from rung 2 in one way that
/// matters here: **a run cannot proceed without them**, so giving one back is
/// never a decision about the output, only about when the session is rebuilt.
/// That is why this has no `Surrendered` state and [`Rung2`] does. A step down
/// the pressure ladder costs a run its rungs and is latched; a spent detector
/// costs a run one reopen and is not.
///
/// `open` is a function pointer rather than a closure so that this stays one
/// plain type rather than a generic over a callable - every instance of it is
/// built from a model directory and a preference, which is exactly what a run
/// has.
pub(crate) struct OnDemand<T: Resident> {
    models: PathBuf,
    preference: Preference,
    /// Which parked session is this one's. See [`session_key`].
    key: residency::Key,
    open: fn(&Path, Preference) -> Result<T, String>,
    held: Option<T>,
}

impl<T: Resident> OnDemand<T> {
    fn new(
        kind: registry::Kind,
        models: &Path,
        preference: Preference,
        open: fn(&Path, Preference) -> Result<T, String>,
    ) -> OnDemand<T> {
        OnDemand {
            models: models.to_path_buf(),
            preference,
            key: session_key(kind, models, preference),
            open,
            held: None,
        }
    }

    /// The session: the one already in memory if there is one, and a new one
    /// otherwise.
    ///
    /// A failure here is the page's, not the run's: [`clean_page`] turns it
    /// into a page that could not be cleaned, which
    /// leaves it queued for the next run.
    pub(crate) fn get(&mut self) -> Result<&mut T, String> {
        if self.held.is_none() {
            self.held = Some(match residency::checkout::<T>(&self.key) {
                Some(resident) => resident,
                None => (self.open)(&self.models, self.preference)?,
            });
        }
        Ok(self.held.as_mut().expect("just opened"))
    }

    /// Give it back if the registry says so - somebody pressed the close
    /// button, or nothing has used it for
    /// [`cleaner_core::registry::ONNX_IDLE_GRACE`]. Answers whether a session
    /// actually went.
    ///
    /// **A drop here is a real drop**, not a hand-back: `spent` is exactly the
    /// state the cache would refuse to park anyway, and routing it through the
    /// cache would only mean the memory went one sweep later than the user was
    /// told.
    fn release_if_spent(&mut self) -> bool {
        let spent = self.held.as_ref().is_some_and(Resident::spent);
        if spent {
            self.held = None;
        }
        spent
    }

    #[cfg(test)]
    fn is_held(&self) -> bool {
        self.held.is_some()
    }
}

impl<T: Resident> Drop for OnDemand<T> {
    /// The end of a run is **not** a reason to unload a model.
    /// Whatever this is still
    /// holding goes back to the residency cache, where the next run, the next
    /// page and the next region edit will find it.
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            residency::checkin(self.key.clone(), held);
        }
    }
}

/// Which parked session a holder means: the kind, the directory the weights
/// were read from, and the accelerator preference they were built under. Two
/// different model directories are two different sessions, and handing back one
/// opened against the other would be answering a question nobody asked.
fn session_key(kind: registry::Kind, models: &Path, preference: Preference) -> residency::Key {
    residency::Key::new(kind, format!("{}|{preference:?}", models.display()))
}

/// Rung 2's session, and the two states that are not "held".
///
/// **It is not opened until a region routes to it.** A manga-LaMa session costs
/// about 2.1 s to build and holds roughly 510 MB resident,
/// and a chapter of
/// flat-paper dialogue never needs one: every region of it routes to rung 0 and
/// passes. Opening the inpainter with the other three would put that 510 MB and
/// those 2.1 s on every run of every chapter, for a rung most of them never
/// reach. So the run pays for it exactly when it uses it.
///
/// **And once opened it is held for the rest of the run**: building
/// costs seconds and running costs one, so a session rebuilt per region would
/// spend most of a job in the loader. It goes when the [`Pipeline`] goes, at the
/// end of the run, the same as the
/// "unload LaMa" pressure step does by hand.
///
/// It is a value of its own rather than three fields on [`Pipeline`] for the
/// same reason [`Cleaner`] is a trait: the escalation loop is written against
/// this, so every rung below 2 can be exercised on a machine that has no
/// weights, with [`Rung2::state`] saying afterwards whether a session was ever
/// asked for. It was one of a pair - rung 3 held its own MI-GAN session on
/// these terms - and it is the only one left.
pub(crate) struct Rung2 {
    /// Where [`INPAINTER`] is looked for, kept because the session that reads
    /// it is not built until something needs it.
    models: PathBuf,
    preference: Preference,
    /// Which parked session is this one's, so that "held for the rest of the
    /// run" has become "held until somebody asks for it back" - the run's end
    /// hands the session to [`cleaner_core::residency`] rather than dropping
    /// it.
    key: residency::Key,
    state: Session<lama::Inpainter>,
}

/// What a lazily-opened rung's session currently is.
///
/// Generic over the engine, because the three facts it records - when the
/// session is built, what "never needed" means, and what happens when the
/// pressure ladder takes it away - are facts about a *lazily opened rung* and
/// not about the model. Rung 3 was the second user of it and is gone; the
/// generic stays, because the alternative is inlining these four states into
/// [`Rung2`] and writing them out again the next time a rung is opened on
/// demand.
enum Session<E> {
    /// Nothing has needed it yet. Not the same as [`Session::Unavailable`]:
    /// this is the state a run of pure rung-0 pages ends in, and it is the one
    /// that says the 510 MB was never paid.
    Unopened,
    Held(Box<Held<E>>),
    /// It was needed, and it could not be had - no weights on this machine, or
    /// no session would build. Recorded so the failure is paid once per run
    /// rather than once per region.
    Unavailable,
    /// The pressure ladder took it back, or refused to open it in the first
    /// place. Distinct from
    /// `Unavailable`, because the weights are fine and the machine is not, and
    /// distinct from `Unopened`, because it must **not** be opened again: a
    /// step down that ladder is a latch, and rebuilding 510 MB on the next
    /// region is how a machine under pressure gets driven back into the
    /// compressor.
    Surrendered,
}

impl Rung2 {
    pub(crate) fn new(models: &Path, preference: Preference) -> Rung2 {
        Rung2 {
            models: models.to_path_buf(),
            preference,
            key: session_key(registry::Kind::Inpainter, models, preference),
            state: Session::Unopened,
        }
    }

    /// The held inpainter, opening it if this is the first region to want one.
    ///
    /// `None` is "this run has no rung 2", not "this region cannot use it". The
    /// two are told apart at the call site because they carry different reasons
    /// into review.
    fn held(&mut self) -> Result<Option<&mut Held<lama::Inpainter>>, String> {
        if matches!(self.state, Session::Unopened) {
            // The parked session first: 2.1 s and 510 MB that a second run, or
            // a hand edit after one, does not have to pay again.
            let resident = residency::checkout::<Held<lama::Inpainter>>(&self.key);
            let opened = match resident {
                Some(held) => Some(held),
                None => open_inpainter(&self.models, self.preference)?,
            };
            self.state = match opened {
                Some(held) => Session::Held(Box::new(held)),
                None => Session::Unavailable,
            };
        }
        Ok(match &mut self.state {
            Session::Held(held) => Some(held),
            _ => None,
        })
    }

    /// Give the session back, under the
    /// pressure ladder. Answers whether there was one to give.
    ///
    /// The state it lands in is [`Session::Surrendered`], which [`Rung2::held`]
    /// does not reopen - so every later region routes down to rung 0, or
    /// is left alone and listed in review, and the run continues. That is what
    /// "each step is recoverable" means here: the run is a rung worse, not over.
    fn surrender(&mut self) -> bool {
        let held = matches!(self.state, Session::Held(_));
        self.state = Session::Surrendered;
        held
    }

    /// Give the session back because **nothing is using it** - the close button
    /// in the loaded-models tab, or
    /// [`cleaner_core::registry::IDLE_GRACE`] with no region having routed here.
    ///
    /// It lands in [`Session::Unopened`] and not [`Session::Surrendered`], and
    /// that is the whole difference between this and the pressure ladder: the
    /// ladder's step is a latch because the machine is short of memory and
    /// rebuilding 510 MB is how it gets driven into the compressor, whereas
    /// this is a session nobody wanted on a machine that is fine. A later
    /// region gets a rung 2 again, and pays the 2.1 s once.
    fn release_if_spent(&mut self) -> bool {
        let spent = match &self.state {
            Session::Held(held) => held.inpainter.spent(),
            _ => false,
        };
        if spent {
            self.state = Session::Unopened;
        }
        spent
    }

    /// Where the held session is running, as the key the interface names the
    /// accelerator under.
    ///
    /// **It opens nothing.** The only caller asks after a run call on this
    /// session has already failed, so the state is `Held` by construction; the
    /// other arms exist because the enum has them, and the CPU is the answer
    /// that claims the least on a path nothing can reach.
    pub(crate) fn accel_key(&self) -> &'static str {
        match &self.state {
            Session::Held(held) => held.inpainter.accel_key(),
            _ => accel::Accelerator::Cpu.label_key(),
        }
    }
}

impl Drop for Rung2 {
    /// A held session goes back to the residency cache, and a **surrendered**
    /// one does not exist to go anywhere: [`Rung2::surrender`] already dropped
    /// it, which is what the pressure ladder asked for.
    fn drop(&mut self) {
        if let Session::Held(held) = std::mem::replace(&mut self.state, Session::Unopened) {
            residency::checkin(self.key.clone(), *held);
        }
    }
}

/// A held model session, with the two facts about it that go into every patch
/// it produces.
struct Held<E> {
    inpainter: E,
    /// Where the session landed, from the engine's own `selection` and not from
    /// the detector's - the models choose their providers separately and a
    /// patch names the one that made it.
    provider: String,
    /// The weights' digest. `None` on every rung below this one because there
    /// is no model to digest; here, a missing digest would be a missing answer
    /// rather than a true one.
    model_sha256: Option<String>,
}

/// **The provider and the digest are parked with the session, not recomputed.**
/// They are facts about *this* session - where it landed, and the bytes it was
/// built from - so a cache that kept the session and threw them away would make
/// the next run read 207 MB off disk to digest weights it never opened.
impl Resident for Held<lama::Inpainter> {
    fn spent(&self) -> bool {
        self.inpainter.spent()
    }
}

impl Pipeline {
    /// **The pressure ladder's trigger.**
    ///
    /// Called once per region, between regions, which is the only moment a step
    /// is safe to take: rule 6's discipline means the last region's buffers are
    /// gone and the next one's do not exist, so what is resident here is the
    /// sessions - and the sessions are what the ladder gives back.
    ///
    /// The signal is [`cleaner_core::memory::pressure`], the kernel's own
    /// `kern.memorystatus_vm_pressure_level`, and **not**
    /// `os_proc_available_memory`, which answers 0 for
    /// a process with no per-process limit - every macOS process this
    /// application will ever be. That measurement is in
    /// [`cleaner_core::memory`]'s own documentation.
    ///
    /// Two steps, and rung 3a's step is missing because rung 3a is refused
    /// before it is ever opened - the order is "refuse and
    /// unload rung 3a, then unload LaMa, then drop to one window in flight":
    ///
    /// - **Unload the inpainter.** Rung 2's held session goes back, and the
    ///   parked copies of it go with it. This step used to unload two model
    ///   rungs because there were two; rung 3 is gone and the step is the same
    ///   step with one fewer session to give.
    /// - **One window in flight**, rule 6's own remedy.
    ///
    /// Both are recoverable, which is the property that makes refusing better
    /// than absorbing: a region with no model routes down to rung 0, or
    /// is left exactly as it was and listed in review with its reason. Nothing
    /// fails, and nothing is half-cleaned.
    fn under_pressure(&mut self) -> Option<&'static str> {
        // **Rung 3a's step, wired at last.** It could not
        // have one before: a run never spawned a sidecar and a region edit
        // killed its own child at the end of the click, so there was never a
        // moment when this process was holding one and a run was going. Now
        // that a child survives the edit that spawned it, there is - and the
        // first step down the ladder is the 4.6 GB one rather than the 510 MB
        // one.
        if registry::any_loaded(registry::Kind::Sidecar) {
            self.ladder.holding_sidecar();
        }
        self.take_pressure(memory::pressure())
    }

    /// [`Pipeline::under_pressure`] against a reading the caller supplies.
    ///
    /// A machine under memory pressure is not a state a test can arrange, and a
    /// ladder nothing has ever climbed is a ladder that works until the first
    /// time it is needed. So the reading is a parameter one call in, and
    /// `a_run_under_pressure_gives_back_the_inpainter_and_keeps_going` drives
    /// the whole pipeline through it on a real page.
    fn take_pressure(&mut self, reading: memory::Pressure) -> Option<&'static str> {
        let step = self.ladder.take(reading)?;
        if step == memory::Step::UnloadInpainter {
            // `surrender` is idempotent, and the session may never have been
            // opened at all.
            self.rung2.surrender();
            // And the parked copy with it. A ladder that gave back the session
            // this run was holding while leaving an idle one of the same kind
            // in the cache would be a ladder that freed nothing on the run that
            // never opened rung 2 - which is the run most likely to be the one
            // that hit the pressure.
            residency::evict(registry::Kind::Inpainter);
        }
        if step == memory::Step::RefuseSidecar {
            // Refusing the rung is only half of it: the child is already up and
            // holding gigabytes, and this step exists to get them back.
            residency::evict(registry::Kind::Sidecar);
        }
        Some(memory::step_key(step))
    }

    /// **Checks the weights and opens nothing.**
    ///
    /// The failure this used to catch by opening three sessions is "the model
    /// files are not on this machine", and that is a question about the
    /// filesystem - asking it by building a session cost a run 100 MB and
    /// several seconds before it had looked at a page. Every other failure a
    /// session can have is one the page reports, through
    /// [`OnDemand::get`], and it already
    /// leaves such a page queued rather than losing it.
    /// Give back every session nothing is using, at a point where dropping one
    /// is safe.
    ///
    /// **The same safe point [`Pipeline::under_pressure`] uses**, and for the
    /// same reason spelled out there: between regions the last region's buffers
    /// are gone and the next one's do not exist, so what is resident is the
    /// sessions. Two things put a session on this list, and
    /// [`cleaner_core::registry`] is what makes them one question:
    ///
    /// - the user pressed the close button on its row in the loaded-models tab;
    /// - nothing has used it for [`cleaner_core::registry::IDLE_GRACE`], which
    ///   in a run that is flowing never happens to the detectors and does
    ///   happen to rung 2 across a stretch of pages that never reach it.
    ///
    /// Nothing here is a latch. A page that needs the detector again opens it
    /// again, which is the honest cost of a close button on a model a run
    /// cannot proceed without.
    fn reap(&mut self) {
        self.detector.release_if_spent();
        self.balloons.release_if_spent();
        self.gate.release_if_spent();
        self.reader.release_if_spent();
        self.rung2.release_if_spent();
        if self.full_rt.as_ref().is_some_and(FullRegions::spent) {
            self.full_rt = None;
        }
        if self.sam.as_ref().is_some_and(sam_ts::SamTsSession::spent) {
            self.sam = None;
        }
        // And what *nobody* is holding: a session parked by the last run or the
        // last region edit, whose grace has elapsed while this run was busy
        // with rungs it never needed.
        residency::sweep();
    }

    pub fn open(models: &Path, preference: Preference) -> Result<Pipeline, OpenError> {
        Self::open_selected(models, preference, DetectorModels::default(), false, None)
    }

    fn open_selected(models: &Path, preference: Preference, detection_models: DetectorModels,
        all_text: bool, app_data: Option<PathBuf>) -> Result<Pipeline, OpenError> {
        for name in detection_models.required(all_text) {
            let path = models.join(name);
            if !path.exists() {
                return Err(OpenError::MissingModel { path });
            }
        }
        Ok(Self::unchecked(models, preference, detection_models, all_text, app_data))
    }

    /// A pipeline for a Clean run, which asks nothing of the detection weights:
    /// no detector opens, so none has to be there. Rung 2's weights are asked
    /// for by the rung itself, and a machine without them declines the
    /// regions that needed it, as a one-pass run does.
    fn open_for_cleaning(models: &Path, preference: Preference, app_data: Option<PathBuf>) -> Pipeline {
        Self::unchecked(models, preference, DetectorModels::default(), false, app_data)
    }

    fn unchecked(models: &Path, preference: Preference, detection_models: DetectorModels,
        all_text: bool, app_data: Option<PathBuf>) -> Pipeline {
        Pipeline {
            detector: OnDemand::new(registry::Kind::TextDetector, models, preference, open_detector),
            balloons: OnDemand::new(
                registry::Kind::BalloonDetector,
                models,
                preference,
                open_balloons,
            ),
            gate: OnDemand::new(registry::Kind::ScriptGate, models, preference, open_gate_bare),
            reader: OnDemand::new(registry::Kind::Ocr, models, preference, open_hayai),
            reads: false,
            detection_models,
            all_text,
            app_data,
            full_rt: None,
            sam: None,
            full_rt_preference: preference,
            sam_preference: preference,
            engine_version: env!("CARGO_PKG_VERSION"),
            rung2: Rung2::new(models, preference),
            ladder: memory::Ladder::new(),
            picks: None,
            bubble_color: None,
            mask_padding: 0,
            outside: OutsideText::Review,
            selection: RunSelection::default(),
            fault: None,
            carried: Carried::default(),
            previous_placement: None,
            remote: None,
            prefetch: None,
            remote_page: None,
            remote_below: Vec::new(),
            remote_ahead: Vec::new(),
            remote_stop: None,
            gate_solid_fill: false,
            area: None,
            #[cfg(test)]
            test_vision: None,
            #[cfg(test)]
            answers_below: 0,
            #[cfg(test)]
            test_lettering: None,
        }
    }

    /// The run's engine picks, which arrive with the command rather than with
    /// the model directory. Separate from [`Pipeline::open`] so that every
    /// other caller of it - `spikes/clean-page`, the tests - goes on routing by
    /// the fit alone rather than silently acquiring the interface's defaults.
    pub fn with_picks(mut self, picks: Picks) -> Pipeline {
        self.picks = Some(picks);
        self
    }

    /// The run's granted cloud stages. Before [`Pipeline::with_model_preferences`],
    /// which does not check a local backend for a stage that will not run here.
    pub(crate) fn with_remote(mut self, remote: Option<crate::inference::run_analysis::RemoteDetection>) -> Pipeline {
        self.remote = remote;
        self
    }

    /// Start the run's cloud analysis ahead of its cleaning, over exactly the
    /// pages the walk will take, in its order, each read from where the walk
    /// will read it. Called once the run's thread has started; nothing without
    /// cloud stages.
    pub(crate) fn prefetch_remote(&mut self, entries: &[PlanEntry]) {
        let Some(remote) = self.remote.take() else { return };
        let mut opened: Option<(&Path, Option<Job>)> = None;
        let pages = entries.iter().map(|entry| {
            // A read, so without the lock (see `lock_job`).
            if opened.as_ref().is_none_or(|(path, _)| *path != entry.job_path.as_path()) {
                opened = Some((entry.job_path.as_path(), Job::open(&entry.job_path).ok()));
            }
            let source = opened.as_ref().and_then(|(_, job)| job.as_ref())
                .and_then(|job| job.source_path(entry.source_idx));
            crate::inference::run_analysis::PrefetchPage { page_index: entry.page_index, source, job_path: Some(entry.job_path.clone()) }
        }).collect();
        self.prefetch = Some(crate::inference::run_analysis::Prefetch::start(remote, pages));
    }

    /// The answer taken ahead for this page ([`Pipeline::remote_ahead`]), if
    /// there is one. A page whose bytes changed since is failed, as a
    /// prefetched one is, rather than sent again. Answers for pages the walk
    /// has passed are dropped.
    fn take_ahead(&mut self, placement: usize, source_sha256: &str)
        -> Option<Result<crate::inference::run_analysis::RemotePage, String>> {
        self.remote_ahead.retain(|(ahead, _, _)| *ahead >= placement);
        let at = self.remote_ahead.iter().position(|(ahead, _, _)| *ahead == placement)?;
        let (_, analyzed, answer) = self.remote_ahead.remove(at);
        Some(if analyzed == source_sha256 { answer } else {
            Err("cloud_run_source_changed: the page changed on disk after it was analyzed".into())
        })
    }

    /// Fill [`Pipeline::remote_below`] for the current page: the answer of
    /// every later page one of its detection windows reads into. Windows only
    /// cross verified joins ([`detection_window`]), so a paginated chapter,
    /// or a strip whose next join is unchecked, gathers nothing. A later page
    /// with no answer (not in this run, or failed) ends the gathering: its
    /// rows read as background, and a failed page says so when the walk
    /// reaches it.
    fn gather_below(&mut self, context: &PageContext<'_>) {
        self.remote_below.clear();
        let pages = context.strip.pages();
        let Some(page) = pages.get(context.placement).copied() else { return };
        let reach = context.page_segments().into_iter()
            .map(|segment| detection_window(context, segment, None).rect.bottom())
            .max().unwrap_or(0);
        for (later, placed) in pages.iter().enumerate().skip(context.placement + 1) {
            if placed.y_offset >= reach { break; }
            let Some(answer) = self.answer_below(later, context) else { break };
            #[cfg(test)]
            { self.answers_below += 1; }
            self.remote_below.push(((placed.x_offset - page.x_offset, placed.y_offset - page.y_offset), answer));
        }
    }

    /// A later page's cloud answer for the rows under the current page:
    /// waited for from the prefetch without taking it, or, on a run with no
    /// prefetch, analyzed now and kept for the walk ([`Pipeline::take_ahead`]).
    /// `None` for a page this run was not granted.
    fn answer_below(&mut self, later: usize, context: &PageContext<'_>)
        -> Option<crate::inference::run_analysis::RemotePage> {
        if let Some(prefetch) = &self.prefetch { return prefetch.peek(later); }
        let remote = self.remote.as_ref()?;
        if !remote.grants_page(later) { return None; }
        if let Some((_, _, answer)) = self.remote_ahead.iter().find(|(ahead, _, _)| *ahead == later) {
            return answer.as_ref().ok().cloned();
        }
        let bytes = context.sources.get(later)?.as_ref().and_then(|path| std::fs::read(path).ok())?;
        let raster = cleaner_core::image::orientation::from_bytes(&bytes).raster(&decode(&bytes).ok()?);
        let answer = remote.analyze_page(later, &bytes, &raster);
        let page = answer.as_ref().ok().cloned();
        self.remote_ahead.push((later, sha256_hex(&bytes), answer));
        page
    }

    fn remote_rt(&self) -> bool {
        self.remote.as_ref().is_some_and(|remote| remote.rt_full())
            || self.prefetch.as_ref().is_some_and(|prefetch| prefetch.rt_full())
    }

    fn remote_sam(&self) -> bool {
        self.remote.as_ref().is_some_and(|remote| remote.sam())
            || self.prefetch.as_ref().is_some_and(|prefetch| prefetch.sam())
    }

    /// Whether a cloud stage contributed to this run's detection: a selected
    /// stage whose answer comes from the cloud rather than from this machine.
    fn remote_detected(&self) -> bool {
        (self.detection_models.sam && self.remote_sam())
            || (self.detection_models.rt_full && self.remote_rt())
    }

    fn with_model_preferences(mut self, settings: &serde_json::Value) -> Result<Pipeline, String> {
        self.detector.preference = model_preference_from(settings, "ctd")?;
        self.detector.key = session_key(registry::Kind::TextDetector, &self.detector.models, self.detector.preference);
        self.balloons.preference = model_preference_from(settings, "rtSmall")?;
        self.balloons.key = session_key(registry::Kind::BalloonDetector, &self.balloons.models, self.balloons.preference);
        self.full_rt_preference = model_preference_from(settings, "rtFull")?;
        self.sam_preference = model_preference_from(settings, "samTs")?;
        self.rung2.preference = model_preference_from(settings, "inpainter")?;
        self.rung2.key = session_key(registry::Kind::Inpainter, &self.rung2.models, self.rung2.preference);
        let host = cleaner_core::runtime::package::Platform::host();
        self.reader.preference = reader_preference(settings, host)?;
        self.reader.key = session_key(registry::Kind::Ocr, &self.reader.models, self.reader.preference);
        for (id, enabled, preference) in [
            ("ctd", self.detection_models.ctd, self.detector.preference),
            ("rtSmall", self.detection_models.rt_small, self.balloons.preference),
            ("rtFull", self.detection_models.rt_full && !self.remote_rt(), self.full_rt_preference),
            ("samTs", self.detection_models.sam && !self.remote_sam(), self.sam_preference),
            ("inpainter", true, self.rung2.preference),
        ] {
            if enabled {
                if let Preference::Force(accelerator) = preference {
                    if !crate::models::supports_model(id, accelerator, host) {
                        return Err(format!("{id} does not support {:?} on this platform", accelerator));
                    }
                }
            }
        }
        Ok(self)
    }

    /// The chosen solid color for either speech bubble or outside text regions.
    pub fn with_bubble_color(mut self, color: Option<[u8; 3]>) -> Pipeline {
        self.bubble_color = color;
        self
    }

    /// The Mask padding the tool window asked for, clamped to the widest it
    /// offers.
    pub fn with_mask_padding(mut self, padding: u32) -> Pipeline {
        self.mask_padding = padding.min(cleaner_core::constants::MAX_MASK_PADDING);
        self
    }

    /// Hold the run's Detect to one area of its page. See [`DetectArea`].
    pub fn with_area(mut self, area: Option<DetectArea>) -> Pipeline {
        self.area = area;
        self
    }

    /// The run's answer for text outside a balloon, from the same row of the
    /// tool window as the picks and separate from [`Pipeline::open`] for the
    /// same reason: a pipeline nobody asked reviews that text, as §3 defaults.
    pub fn with_outside(mut self, outside: OutsideText) -> Pipeline {
        self.outside = outside;
        self
    }

    pub fn with_selection(mut self, mut selection: RunSelection) -> Pipeline {
        // Hayai reads every language manga-ocr does and more; one reader is enough.
        self.reads = selection.reader && hayai_installed(&self.reader.models);
        if self.reads {
            selection.ocr_rescue = false;
        }
        if selection.ocr_rescue {
            if let Some(reason) = unavailable_ocr_reason(&self.gate.models) {
                events::notice(
                    "notice.run.ocrRescueUnavailable",
                    serde_json::json!({ "reason": reason }),
                    "warn",
                );
                selection.ocr_rescue = false;
            }
        }
        self.selection = selection;
        self.gate.open = if selection.ocr_rescue { open_gate_with_ocr } else { open_gate_bare };
        if selection.ocr_rescue {
            self.gate.key = residency::Key::new(
                registry::Kind::ScriptGate,
                format!("{}|{:?}|ocr", self.gate.models.display(), self.gate.preference),
            );
        }
        self
    }

    /// Detect one segment crop. `page_offset` is the crop's origin on its
    /// page.
    ///
    /// **The cleaning units are text groups** ([`cleaner_core::text_groups`]),
    /// whichever models ran and wherever they ran. The lettering pixels are
    /// SAM-TS-L's mask when it is selected, else CTD's segmentation at its
    /// mask threshold; Ogkalu text boxes and CTD boxes anchor groups of those
    /// pixels, bubble boxes wall them in, and what no box claims is grouped by
    /// layout and cleaned as a region of its own. A text box no pixel covers (every box,
    /// on an Ogkalu-only run) gets a bounded ink estimate under the box, taken
    /// once: never the box itself, never a pixel a neighbour already has. The
    /// analysis preview calls the same grouping over SAM-TS-L's mask.
    fn detect_segment(
        &mut self,
        crop: &Raster,
        segment: usize,
        page_offset: (i64, i64),
        scope: &str,
    ) -> Result<SegmentDetection, String> {
        // Test runs stand a stub in for the models: its boxes and segmentation
        // are CTD's, its balloons Ogkalu's, its lettering SAM-TS-L's.
        #[cfg(test)]
        let staged = self.test_vision.map(|vision| {
            ((vision.balloons)(crop, segment), (vision.detect)(crop, segment),
                self.test_lettering.map(|lettering| lettering(crop, segment)))
        });
        #[cfg(not(test))]
        let staged = { let _ = segment; None };
        let (mut balloons, mut detection, lettering) = match staged {
            Some(staged) => staged,
            None => self.run_models(crop, page_offset)?,
        };
        // CTD-only has no bubble detector; page geometry can still answer the
        // inside/outside question. Other combinations retain RT context.
        if !self.detection_models.rt_small && !self.detection_models.rt_full { balloons.clear(); }
        let ctd = if self.detection_models.ctd { std::mem::take(&mut detection.boxes) } else { Vec::new() };
        let (pixels, pixel_model) = match lettering {
            Some(mask) => (Some(mask), EvidenceModel::SamTsL),
            None if self.detection_models.ctd => (Some(detection.segmentation.lettering_mask()), EvidenceModel::Ctd),
            None => (None, EvidenceModel::SamTsL),
        };
        let (grouping, evidence) =
            self.group_segment(crop, page_offset, scope, pixels.as_deref(), pixel_model, &balloons, &ctd)?;
        // SAM's pixels are the lettering the gate reads too; CTD's own
        // segmentation already is, and an estimate is painted in per region.
        if let (Some(mask), EvidenceModel::SamTsL) = (pixels, pixel_model) {
            detection.segmentation.levels = mask;
        } else if !self.detection_models.ctd {
            detection.segmentation.levels.fill(0);
        }
        detection.boxes = ctd;
        Ok(SegmentDetection { balloons, detection, grouping, evidence })
    }

    /// Run the selected models over one segment crop: Ogkalu's boxes, CTD's
    /// boxes and segmentation (empty when CTD is not selected), and SAM-TS-L's
    /// mask when it is selected.
    fn run_models(
        &mut self,
        crop: &Raster,
        page_offset: (i64, i64),
    ) -> Result<ModelAnswers, String> {
        let remote = self.remote_page.as_ref()
            .map(|page| remote_for_crop(page, &self.remote_below, crop.width, crop.height, page_offset));
        let balloons = if self.detection_models.rt_small {
            let requested_key = requested_accel_key(self.balloons.preference);
            let session = self.balloons.get().inspect_err(|detail| {
                self.fault = Some(EngineFault { model_key: accel::BALLOON.label_key,
                    accel_key: requested_key, detail: detail.clone() });
            })?;
            let boxes = session.detect(crop);
            ran(boxes, &*session, &accel::BALLOON, &mut self.fault)?
        } else if self.detection_models.rt_full && self.remote_rt() {
            remote.as_ref().map(|(boxes, _)| boxes.clone()).ok_or("cloud analysis missing for this page")?
        } else if self.detection_models.rt_full {
            if self.full_rt.is_none() {
                let app_data = self.app_data.as_deref().ok_or("App model directory unavailable")?;
                let path = crate::model_workflows::full_rt_path(app_data)
                    .ok_or("The Ogkalu comic text & bubble detector (Full) graph is not installed or failed SHA-256")?;
                let model = FullRegions::open(&path, self.full_rt_preference).inspect_err(|detail| {
                    self.fault = Some(EngineFault { model_key: accel::FULL_RT.label_key,
                        accel_key: requested_accel_key(self.full_rt_preference), detail: detail.clone() });
                })?.0;
                report_provider(crate::models::RT_FULL_NAME, model.selection());
                self.full_rt = Some(model);
            }
            self.full_rt.as_mut().unwrap().detect_halves(crop)?
        } else { Vec::new() };
        let detection = if self.detection_models.ctd {
            let requested_key = requested_accel_key(self.detector.preference);
            let session = self.detector.get().inspect_err(|detail| {
                self.fault = Some(EngineFault { model_key: accel::DETECTOR.label_key,
                    accel_key: requested_key, detail: detail.clone() });
            })?;
            let output = session.detect(crop);
            ran(output, &*session, &accel::DETECTOR, &mut self.fault)?
        } else {
            Detection {
                boxes: Vec::new(),
                segmentation: Segmentation { width: crop.width, height: crop.height,
                    levels: vec![0; crop.width as usize * crop.height as usize],
                    fit: Letterbox::fit(crop.width, crop.height) },
            }
        };
        let lettering = if self.detection_models.sam && self.remote_sam() {
            Some(remote.and_then(|(_, mask)| mask).ok_or("cloud analysis mask missing for this page")?)
        } else if self.detection_models.sam {
            let backend = match self.sam_preference {
                Preference::Force(accel::Accelerator::WebGpu) => accel::Accelerator::WebGpu,
                Preference::Automatic | Preference::CpuOnly | Preference::Force(accel::Accelerator::Cpu) => accel::Accelerator::Cpu,
                _ => return Err(format!("The SAM-TS-L lettering mask does not support the requested backend {:?}", self.sam_preference)),
            };
            if self.sam.is_none() {
                let app_data = self.app_data.as_deref().ok_or("App model directory unavailable")?;
                let dir = crate::model_workflows::graph_dir(app_data).ok_or("The SAM-TS-L lettering mask graphs are not installed")?;
                cleaner_core::residency::evict_for_sam();
                self.sam = Some(sam_ts::SamTsSession::open_on(&dir, backend, &sam_ts::Cancellation::default()).inspect_err(|detail| {
                    self.fault = Some(EngineFault { model_key: accel::DETECTOR.label_key,
                        accel_key: backend.label_key(), detail: detail.clone() });
                })?);
            }
            Some(self.sam.as_mut().unwrap().infer(crop, &sam_ts::Cancellation::default())?.mask)
        } else { None };
        Ok((balloons, detection, lettering))
    }

    /// Group one segment's evidence and encode its evidence file. `scope`
    /// names the page (source digest and page id), so no two pages' group ids
    /// collide.
    #[allow(clippy::too_many_arguments)]
    fn group_segment(
        &self,
        crop: &Raster,
        page_offset: (i64, i64),
        scope: &str,
        mask: Option<&[u8]>,
        pixel_model: EvidenceModel,
        balloons: &[cleaner_core::balloon::BalloonBox],
        ctd: &[DetBox],
    ) -> Result<(Grouping, EvidenceBlob), String> {
        let mut inputs = text_groups::Inputs::new(crop.width, crop.height, mask);
        inputs.pixel_model = pixel_model;
        inputs.page = Some(crop);
        inputs.origin = page_offset;
        inputs.scope = scope.to_owned();
        inputs.rt = balloons;
        inputs.ctd = ctd;
        inputs.models = self.model_uses();
        inputs.seams = self.segment_seams(crop, page_offset);
        let grouping = text_groups::group(&inputs)?;
        let evidence = grouping.evidence(mask)?;
        Ok((grouping, evidence))
    }

    /// Every detection model this run uses, where it runs, and the spatial
    /// input it sees.
    fn model_uses(&self) -> Vec<ModelUse> {
        let mut uses = Vec::new();
        if self.detection_models.sam {
            uses.push(if self.remote_sam() {
                ModelUse::new(EvidenceModel::SamTsL, Execution::Cloud, SpatialInput::OverlappingCloudTiles)
            } else {
                ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole)
            });
        }
        if self.detection_models.rt_full {
            uses.push(if self.remote_rt() {
                ModelUse::new(EvidenceModel::OgkaluFull, Execution::Cloud, SpatialInput::OverlappingCloudTiles)
            } else {
                ModelUse::new(EvidenceModel::OgkaluFull, Execution::Local, SpatialInput::Halves)
            });
        }
        if self.detection_models.rt_small {
            uses.push(ModelUse::new(EvidenceModel::OgkaluSmall, Execution::Local, SpatialInput::Whole));
        }
        if self.detection_models.ctd {
            uses.push(ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole));
        }
        uses
    }

    /// Where this crop's evidence was cut: the cloud plan's core boundaries
    /// (where the stitched mask changes owner) for a cloud stage, and the
    /// local full RT-DETR's two halves.
    fn segment_seams(&self, crop: &Raster, (dx, dy): (i64, i64)) -> Vec<Seam> {
        let mut seams = Vec::new();
        let cloud = (self.detection_models.sam && self.remote_sam())
            || (self.detection_models.rt_full && self.remote_rt());
        if let Some(page) = self.remote_page.as_ref().filter(|_| cloud) {
            // A later page's answer under a join was cut at the join as well
            // as at its own tiles.
            let pieces = std::iter::once(((0, 0), page))
                .chain(self.remote_below.iter().map(|(at, below)| (*at, below)));
            for ((ax, ay), piece) in pieces {
                let (xs, ys) = cleaner_core::cloud_tiles::core_boundaries(piece.width, piece.height);
                seams.extend(xs.into_iter().map(|x| i64::from(x) + ax - dx)
                    .filter(|&x| x > 0 && x < i64::from(crop.width)).map(Seam::Vertical));
                seams.extend(ys.into_iter().map(|y| i64::from(y) + ay - dy)
                    .chain((ay != 0).then_some(ay - dy))
                    .filter(|&y| y > 0 && y < i64::from(crop.height)).map(Seam::Horizontal));
            }
        }
        if self.detection_models.rt_full && !self.remote_rt() && crop.height >= 2 {
            seams.push(Seam::Horizontal(i64::from(crop.height / 2)));
        }
        seams
    }

    /// The gate's verdict for a region whose in/out answer the caller already
    /// has (`inside`, from [`cleaner_core::balloon::in_bubble`]). Under the All
    /// text policy no gate runs and every region is opted in; `inside` has
    /// still been computed by then, and still decides the region's engine pick
    /// and what its stored detection records.
    fn judge_region(
        &mut self,
        crop: &Raster,
        segmentation: &cleaner_core::detect::Segmentation,
        region: &cleaner_core::detect::Region,
        detected: cleaner_core::balloon::Detected,
        inside: bool,
    ) -> Result<Verdict, String> {
        #[cfg(test)]
        if let Some(vision) = self.test_vision {
            return Ok((vision.judge)(region));
        }
        if self.all_text { return Ok(self.read_check(crop, region, detected, Verdict::OptedIn)); }
        let session = self.gate.get()?;
        let reader_missing = self.selection.ocr_rescue && !session.has_reader();
        let judged = session.judge(crop, segmentation, region, detected, inside, self.outside);
        let verdict = ran(judged, &*session, &accel::SCRIPT_ID, &mut self.fault)?;
        if reader_missing {
            self.selection.ocr_rescue = false;
            self.gate.open = open_gate_bare;
            self.gate.key = session_key(registry::Kind::ScriptGate, &self.gate.models, self.gate.preference);
        }
        Ok(self.read_check(crop, region, detected, verdict))
    }

    /// What a Hayai reading does to a verdict: a failed in-balloon verdict may
    /// be rescued ([`cleaner_core::gate::rescued_by`]), and a region about to be
    /// cleaned with no script reading or no text box behind it is held back
    /// when it reads as art ([`cleaner_core::gate::text_checked`]). Every other
    /// verdict is not read.
    /// The reader is optional, so a reader that will not open turns reading
    /// off for the run with a notice, and a failed read changes nothing.
    fn read_check(
        &mut self,
        crop: &Raster,
        region: &cleaner_core::detect::Region,
        detected: cleaner_core::balloon::Detected,
        verdict: Verdict,
    ) -> Verdict {
        if !self.reads {
            return verdict;
        }
        let rescue = cleaner_core::gate::rescue_applies(&verdict, detected);
        let shape = cleaner_core::gate::shape_rescue_applies(&verdict, detected);
        let overrule = cleaner_core::gate::overrule_applies(&verdict, detected);
        let boxless = region.group.as_ref()
            .is_some_and(|held| held.group.origin == cleaner_core::text_groups::GroupOrigin::MaskLayout);
        let check = verdict == Verdict::OptedIn || (boxless && verdict.cleans());
        if !rescue && !shape && !overrule && !check {
            return verdict;
        }
        let reading = match self.reader.get() {
            Ok(reader) => reader.read(crop, region.text_bounds()).ok(),
            Err(error) => {
                eprintln!("manga-cleaner: the Hayai text reader could not be opened, cleaning without it: {error}");
                events::notice(
                    "notice.run.ocrRescueUnavailable",
                    serde_json::json!({ "reason": error.chars().take(180).collect::<String>() }),
                    "warn",
                );
                self.reads = false;
                None
            }
        };
        if rescue {
            cleaner_core::gate::rescued_by(verdict, reading.as_ref())
        } else if shape {
            cleaner_core::gate::shape_rescued_by(verdict, reading.as_ref())
        } else if overrule {
            cleaner_core::gate::overruled_by(verdict, reading.as_ref())
        } else {
            cleaner_core::gate::text_checked(verdict, reading.as_ref())
        }
    }
}

/// A page's cloud answer, cut to one detection crop. `offset` is the crop's
/// origin in page coordinates. `below` is the later pages' answers a long
/// strip's crop reaches into across a join, each at its origin relative to
/// this page ([`Pipeline::remote_below`]). Boxes are clipped to the crop and
/// dropped when nothing is left; a box a join cut in two, one half facing the
/// other across it, is one box again, as a model reading the crop whole would
/// have found it. Mask pixels no answer covers read as background.
pub(crate) fn remote_for_crop(
    remote: &crate::inference::run_analysis::RemotePage,
    below: &[((i64, i64), crate::inference::run_analysis::RemotePage)],
    width: u32,
    height: u32,
    offset: (i64, i64),
) -> (Vec<cleaner_core::balloon::BalloonBox>, Option<Vec<u8>>) {
    use cleaner_core::balloon::BalloonBox;
    let pieces: Vec<((i64, i64), &crate::inference::run_analysis::RemotePage)> = std::iter::once(((0, 0), remote))
        .chain(below.iter().map(|(at, page)| (*at, page))).collect();
    // Every piece's boxes in this page's coordinates, then each join's cut
    // boxes joined: a box ending on the join above it and one of the same
    // class starting on it below, overlapping across it by half the narrower.
    let mut lists: Vec<Vec<BalloonBox>> = pieces.iter().map(|((ax, ay), page)| page.boxes.iter().map(|item| BalloonBox {
        rect: Rect::new(item.rect.x + ax, item.rect.y + ay, item.rect.w, item.rect.h), ..*item
    }).collect()).collect();
    let tolerance = strip::SPLIT_EDGE_TOLERANCE;
    for upper in 0..lists.len().saturating_sub(1) {
        let join = pieces[upper + 1].0.1;
        let above = std::mem::take(&mut lists[upper]);
        for cut in above {
            let lower = (cut.rect.bottom() >= join - tolerance).then(|| lists[upper + 1].iter().position(|other| {
                let shared = cut.rect.right().min(other.rect.right()) - cut.rect.x.max(other.rect.x);
                other.class == cut.class && other.rect.y <= join + tolerance
                    && shared * 2 >= i64::from(cut.rect.w.min(other.rect.w))
            })).flatten();
            match lower {
                Some(at) => {
                    let other = lists[upper + 1].remove(at);
                    lists[upper + 1].push(BalloonBox {
                        rect: rect_hull(cut.rect, other.rect), class: cut.class, score: cut.score.max(other.score),
                    });
                }
                None => lists[upper].push(cut),
            }
        }
    }
    let (dx, dy) = offset;
    let boxes = lists.into_iter().flatten().filter_map(|item| {
        let x0 = (item.rect.x - dx).max(0);
        let y0 = (item.rect.y - dy).max(0);
        let x1 = (item.rect.right() - dx).min(i64::from(width));
        let y1 = (item.rect.bottom() - dy).min(i64::from(height));
        (x1 > x0 && y1 > y0).then(|| BalloonBox {
            rect: Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32),
            class: item.class,
            score: item.score,
        })
    }).collect();
    let mask = remote.mask.as_ref().map(|_| {
        let mut out = vec![0u8; width as usize * height as usize];
        for ((ax, ay), page) in &pieces {
            let Some(pixels) = page.mask.as_ref() else { continue };
            for y in 0..height as i64 {
                let py = y + dy - ay;
                if py < 0 || py >= i64::from(page.height) { continue; }
                for x in 0..width as i64 {
                    let px = x + dx - ax;
                    if px < 0 || px >= i64::from(page.width) { continue; }
                    out[(y * i64::from(width) + x) as usize] = pixels[(py * i64::from(page.width) + px) as usize];
                }
            }
        }
        out
    });
    (boxes, mask)
}

/// The stable code at the front of a cloud failure, for the notice.
fn cloud_failure_code(detail: &str) -> &str {
    let code = detail.split(':').next().unwrap_or("").trim();
    if !code.is_empty() && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') { code } else { "cloud_analysis_failed" }
}

/* The three sessions a page needs before it can route anything. Free
 * functions rather than closures because `OnDemand` holds a function
 * pointer - see the type. */

/// A detector holder for a caller that is not a run.
///
/// [`crate::region`]'s bench wants exactly what a pipeline wants - the resident
/// session if there is one, a new one if there is not, and a hand-back at the
/// end - and the way to make a hand edit and a run share one session is for
/// both to ask the same type for it rather than for the bench to open its own.
pub(crate) fn detector_on_demand(models: &Path, preference: Preference) -> OnDemand<Detector> {
    OnDemand::new(registry::Kind::TextDetector, models, preference, open_detector)
}

fn open_detector(models: &Path, preference: Preference) -> Result<Detector, String> {
    let model = Detector::open(&models.join(DETECTOR), preference).map_err(|e| e.to_string())?;
    report_provider(crate::models::CTD_NAME, model.selection());
    Ok(model)
}

fn open_balloons(
    models: &Path,
    preference: Preference,
) -> Result<cleaner_core::balloon::BalloonDetector, String> {
    let model = cleaner_core::balloon::BalloonDetector::open(&models.join(BALLOONS), preference)
        .map_err(|e| e.to_string())?;
    report_provider(crate::models::RT_SMALL_NAME, model.selection());
    Ok(model)
}

fn report_provider(model: &str, selection: &accel::Selection) {
    if let Some(declined) = selection.declined {
        if declined.wanted != selection.accelerator {
            let name = |accelerator: accel::Accelerator| match accelerator {
                accel::Accelerator::Cpu => "CPU",
                accel::Accelerator::CoreMl => "CoreML",
                accel::Accelerator::DirectMl => "DirectML",
                accel::Accelerator::Cuda => "CUDA",
                accel::Accelerator::TensorRt => "TensorRT",
                accel::Accelerator::Rocm => "ROCm",
                accel::Accelerator::OpenVino => "OpenVINO",
                accel::Accelerator::WebGpu => "WebGPU",
                accel::Accelerator::Xnnpack => "XNNPACK",
            };
            events::notice("notice.run.accelFallback", serde_json::json!({
                "model": model,
                "requested": name(declined.wanted),
                "effective": name(selection.accelerator),
                "reason": "the requested backend could not build this model's session",
                "reasonKey": declined.reason_key,
            }), "warn");
        }
    }
}

/// The bare gate used when the optional rescue was not selected.
fn open_gate_bare(models: &Path, preference: Preference) -> Result<ScriptGate, String> {
    ScriptGate::open(&models.join(GATE_MODEL), &models.join(GATE_LABELS), preference)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
fn open_gate(models: &Path, preference: Preference) -> Result<ScriptGate, String> {
    open_gate_with_ocr(models, preference)
}

fn unavailable_ocr_reason(models: &Path) -> Option<String> {
    let missing: Vec<_> = OCR_MODELS.iter().filter(|name| !models.join(name).exists()).copied().collect();
    if !missing.is_empty() {
        return Some(format!("Missing reader files: {}", missing.join(", ")));
    }
    OCR_MODELS.iter().find_map(|name| {
        match std::fs::metadata(models.join(name)) {
            Ok(metadata) if metadata.len() == 0 => Some(format!("Empty reader file: {name}")),
            Err(error) => Some(format!("Reader file {name} cannot be read: {error}")),
            _ => None,
        }
    })
}

/// Attach the selected reader if it opens. A missing or unreadable optional
/// reader turns rescue off with a notice and keeps the bare gate available.
fn open_gate_with_ocr(models: &Path, preference: Preference) -> Result<ScriptGate, String> {
    let gate = ScriptGate::open(&models.join(GATE_MODEL), &models.join(GATE_LABELS), preference)
        .map_err(|e| e.to_string())?;
    if let Some(reason) = unavailable_ocr_reason(models) {
        events::notice(
            "notice.run.ocrRescueUnavailable",
            serde_json::json!({ "reason": reason }),
            "warn",
        );
        return Ok(gate);
    }
    match cleaner_core::gate::Ocr::open(
        &models.join(OCR_ENCODER),
        &models.join(OCR_DECODER),
        &models.join(OCR_VOCAB),
        preference,
    ) {
        Ok(ocr) => Ok(gate.with_ocr(ocr)),
        Err(error) => {
            eprintln!("manga-cleaner: the Japanese text reader could not be opened, cleaning without it: {error}");
            events::notice(
                "notice.run.ocrRescueUnavailable",
                serde_json::json!({ "reason": error.to_string().chars().take(180).collect::<String>() }),
                "warn",
            );
            Ok(gate)
        }
    }
}

/// Build rung 2's session, and digest the weights it was built from.
///
/// The digest is taken **before** the session, so the 207 MB read and the
/// 510 MB session are not resident at the same moment. It is the only place a
/// run reads the weights itself, and it happens once.
///
/// **Weights that are not there end the attempt here**, rather than at the
/// session builder that would also have refused them. The wasted work is the
/// smaller half of the reason. The larger one is that
/// [`cleaner_core::accel::open_session`] asks the ONNX Runtime which providers
/// it carries before it opens any file, and on a machine where the runtime
/// itself has never been loaded that question does not return an error - `ort`
/// panics inside its own accessor. A run loads the runtime before it reaches
/// this function and gives up if it cannot, so nothing in the application can
/// arrive here in that state; a test asserting what a machine with no weights
/// does can, and the answer it is asserting is the `None` below.
fn open_inpainter(models: &Path, preference: Preference) -> Result<Option<Held<lama::Inpainter>>, String> {
    let model = models.join(INPAINTER);
    // Read and digested in one expression so the 207 MB is dropped at the end
    // of it, which is the property the paragraph above is about.
    let Some(model_sha256) = std::fs::read(&model).ok().map(|bytes| sha256_hex(&bytes)) else {
        return Ok(None);
    };
    let inpainter = match lama::Inpainter::open(&model, preference) {
        Ok(inpainter) => inpainter,
        Err(error) if matches!(preference, Preference::Force(_)) => return Err(error.to_string()),
        Err(_) => return Ok(None),
    };
    report_provider("LaMa Manga", inpainter.selection());
    let provider = format!("{:?}", inpainter.selection().accelerator).to_lowercase();
    Ok(Some(Held { inpainter, provider, model_sha256: Some(model_sha256) }))
}

impl Cleaner for Pipeline {
    fn clean_page(
        &mut self,
        page_id: &str,
        bytes: &[u8],
        ceiling: Engine,
        context: &PageContext,
    ) -> Result<PageOutcome, String> {
        self.analyse_page(page_id, bytes, ceiling, context, false, None)
    }

    /// The same walk as [`Cleaner::clean_page`], to the same fit, and one line
    /// shorter: see [`Pipeline::analyse_page`]. Whatever differs between
    /// detect-then-clean and one pass has to come from somewhere other than
    /// two copies of the detection.
    fn detect_page(
        &mut self,
        page_id: &str,
        bytes: &[u8],
        ceiling: Engine,
        context: &PageContext,
    ) -> Result<PageOutcome, String> {
        self.analyse_page(page_id, bytes, ceiling, context, true, None)
    }

    fn cloud_detection(&self) -> bool { self.remote_detected() }

    fn area_only(&self) -> bool { self.area.is_some() }

    fn detect_current_page(
        &mut self, page_id: &str, bytes: &[u8], ceiling: Engine,
        context: &PageContext, job: &Job,
    ) -> Result<PageOutcome, String> {
        let input = if self.remote_detected() {
            Some(crate::inference::run_analysis::page_input(job, context.placement, bytes)?)
        } else { None };
        self.analyse_page(page_id, bytes, ceiling, context, true, input)
    }

    /// The ids are the detections' own, so the page's is not needed.
    fn clean_detections(
        &mut self,
        _page_id: &str,
        bytes: &[u8],
        ceiling: Engine,
        context: &PageContext,
        detections: &[LoadedDetection],
    ) -> Result<DetectionsOutcome, String> {
        self.clean_stored(bytes, ceiling, context, detections)
    }

    fn last_fault(&self) -> Option<EngineFault> {
        self.fault.clone()
    }

    fn stop_reason(&self) -> Option<String> {
        self.remote_stop.clone()
    }
}

impl Pipeline {
    /// `spikes/clean-page`'s body, region for region, with the counting
    /// replaced by records the manifest can hold - and with rules 3 to 5
    /// around it.
    ///
    /// **Detection is per segment** (rule 3). A segment is a range of strip
    /// rows and this build's segments are cut at page boundaries, so a page is
    /// one segment unless rule 2 split it - which happens exactly when a source
    /// is a long strip in its own right. That case is not a nicety: an
    /// 800×12000 source letterboxed whole into the detector's 1024² input is
    /// scaled by k ≈ 11.7 and there is nothing left to detect. Cut into six
    /// segments it is k ≈ 2.8, which is an ordinary page.
    ///
    /// **Ownership is decided on the global list** (rule 3's last paragraph),
    /// never per segment, and the evidence for it is this segment's detections
    /// together with the previous segment's - the only other segment whose
    /// detection range can overlap this one's.
    ///
    /// **Every read is bounded by a decode window** (rule 4), which ends in
    /// [`Strip::clamp`] (rule 1). The window is what the candidate ladder grows
    /// inside, so the ring it is measured against is inside it too.
    ///
    /// **Detect small, measure large** (rule 5): the detector sees the 1024²
    /// letterbox of the crop, and the gate, the ring statistics, the fit and
    /// the fill all run on the crop's own pixels.
    ///
    /// **`detect_only` is Detect**, and it stops each region at the one point
    /// the walk above is separable: after the fit and the pick, before the
    /// ladder. Everything before that line is shared, so a stored detection is
    /// exactly the region a one-pass run would have cleaned, with exactly the
    /// mask it would have cleaned it through.
    fn analyse_page(
        &mut self,
        page_id: &str,
        bytes: &[u8],
        ceiling: Engine,
        context: &PageContext,
        detect_only: bool,
        input: Option<Raster>,
    ) -> Result<PageOutcome, String> {
        // Whatever the last page faulted on is the last page's. `last_fault` is
        // read straight after a failed `clean_page` and must answer for that
        // page only, so the slot is emptied before anything can fill it.
        self.fault = None;
        let page = match input { Some(page) => page, None => cleaner_core::image::orientation::from_bytes(bytes).raster(&decode(bytes).map_err(|e| e.to_string())?) };
        cleaner_core::image::color::validate_editing(&page).map_err(|e| e.to_string())?;
        let source_sha256 = sha256_hex(bytes);
        let group_scope = format!("{source_sha256}|{page_id}");
        // The cloud stages come first, once per page and before any local
        // model, so a refusal costs nothing here. In a run they were done ahead
        // by the prefetch and this only takes the answer, waiting when cleaning
        // has caught up with the producer. A failure fails this page alone, and
        // is reported here, at its page: it is not retried, and not analyzed
        // locally instead.
        self.remote_page = None;
        self.remote_below.clear();
        let ahead = self.take_ahead(context.placement, &source_sha256);
        let answer = match (ahead, &self.prefetch, &self.remote) {
            (Some(answer), _, _) => Some(answer),
            (None, Some(prefetch), _) => Some(prefetch.take(context.placement, &source_sha256)),
            (None, None, Some(remote)) => Some(remote.analyze_page(context.placement, bytes, &page)),
            (None, None, None) => None,
        };
        if let Some(answer) = answer {
            match answer {
                Ok(answer) => {
                    self.remote_page = Some(answer);
                    self.gather_below(context);
                }
                Err(detail) => {
                    // Every failed page says so: in a chapter run each is a
                    // different page left as it was.
                    let code = cloud_failure_code(&detail);
                    events::notice(
                        "cloud.analysis.run.pageFailed",
                        serde_json::json!({ "page": context.placement + 1, "code": code }),
                        "warn",
                    );
                    if crate::inference::run_analysis::stops_run(&detail) {
                        self.remote_stop = Some(code.to_owned());
                    }
                    return Err(format!("cloud_analysis_failed: {detail}"));
                }
            }
        }
        // A page begins by giving back whatever the last one finished with.
        // The first thing a run does is therefore to open what it needs and
        // nothing else, and a run that has been asked for a model back gets no
        // further than one page holding it.
        self.reap();
        // **Every run call below goes through [`ran`].** The failure it exists
        // for is a session that ran once and then died - a Windows driver
        // watchdog resetting the device mid-inference is the shape of it - and
        // the two things that were missing are the same at all four sites: the
        // dead session was kept and handed to the next page, and the error text
        // was dropped on the floor.
        // Page-level statistics, measured once and handed to every region.
        // **Per page and not per segment**, because the noise floor is one of
        // the two bounded exceptions to "no global operations" and it is
        // bounded at the page.
        let noise = fit::page_noise_sigma(&page);
        // The run's provider, from the detector's session - which is opened
        // here rather than at the first segment because this is where the name
        // is needed, and the first segment wants it anyway.
        #[cfg(test)]
        let provider = if self.test_vision.is_some() {
            "test".to_owned()
        } else if !self.detection_models.ctd {
            "ort-cpu".to_owned()
        } else {
            format!("{:?}", self.detector.get()?.selection().accelerator).to_lowercase()
        };
        #[cfg(not(test))]
        let provider = if self.detection_models.ctd {
            format!("{:?}", self.detector.get()?.selection().accelerator).to_lowercase()
        } else { "ort-cpu".to_owned() };
        // Rule 4's engine-context term is a property of the rung that could
        // run, and the ceiling is what decides that.
        let engine_context = if rung(ceiling) >= rung(Engine::Lama) {
            EngineContext::Local
        } else {
            EngineContext::None
        };
        let cuts = context.cuts();

        let (page_x, page_y) = context.origin();
        // A Detect held to an area reads a window around it and nothing else
        // of the page, so the models see its lettering at the page's own
        // resolution. Both rectangles are in page pixels.
        let area = self.area.filter(|_| detect_only).map(|area| {
            let area = area.on_page(page.width, page.height);
            (area, DetectArea::window(area, page.width, page.height))
        });
        let within = area.map(|(_, window)| Rect::new(window.x + page_x, window.y + page_y, window.w, window.h));
        let mut at = PageEmission {
            page_id,
            page: &page,
            context,
            origin: (page_x, page_y),
            noise,
            engine_context,
            ceiling,
            detect_only,
            source_sha256: &source_sha256,
            provider,
            outcome: PageOutcome::default(),
            index: 0,
        };
        // What the previous page's segments left is evidence here only when
        // this page follows that one in the same run.
        if self.previous_placement.and_then(|p| p.checked_add(1)) != Some(context.placement) {
            self.carried = Carried::default();
        }
        let Carried {
            found: mut previous,
            deferred: mut pending,
            candidates: mut held,
            mut claims,
            mut listed,
        } = std::mem::take(&mut self.carried);
        // A fresh edge box follows the normal path. Its result is withdrawn
        // only if the next crop confirms that it was a clipped fragment.
        // A clipped box can have consumed an index without producing a row.
        // Only an appended row has a position that a continuation may remove.
        let mut clipped_adopted = Vec::<(Rect, Option<usize>, Option<usize>)>::new();
        let page_segments = context.page_segments();
        let first_segment = page_segments.first().map(|segment| segment.index);
        let last_segment = page_segments.last().map(|segment| segment.index);
        // The first strip row the next page's segments process: a record
        // starting there or below is theirs.
        let page_end = page_segments.last().map_or(page_y, |segment| i64::from(segment.end));

        for segment in page_segments {
            let mut awaiting_continuation = std::mem::take(&mut clipped_adopted);
            let window = detection_window(context, segment, within);
            let requested = window.requested;
            let rect = window.rect;
            if rect.w == 0 || rect.h == 0 { continue; }
            let bounded = context.read_window(window, &page).or_else(|_| {
                let placed = context.strip.pages().get(context.placement)
                    .ok_or_else(|| "page is outside strip".to_owned())?;
                let page_rect = placed.rect();
                let x = requested.x.max(page_rect.x);
                let y = requested.y.max(page_rect.y);
                let right = requested.right().min(page_rect.right());
                let bottom = requested.bottom().min(page_rect.bottom());
                let local = Rect::new(x, y, (right - x).max(0) as u32, (bottom - y).max(0) as u32);
                context.read_window(strip::DecodeWindow {
                    rect: local,
                    requested,
                    pad: strip::EdgePad::Replicate,
                }, &page)
            })?;
            let (crop_x, crop_y) = bounded.origin;

            let SegmentDetection { balloons, mut detection, grouping, evidence: segment_evidence } =
                self.detect_segment(&bounded.raster, segment.index, (crop_x - page_x, crop_y - page_y), &group_scope)?;
            if !self.detection_models.ctd {
                #[cfg(test)]
                let real_models = self.test_vision.is_none();
                #[cfg(not(test))]
                let real_models = true;
                if real_models && ((self.detection_models.sam && self.remote_sam())
                    || (!self.detection_models.sam && self.detection_models.rt_full && self.remote_rt())) {
                    at.provider = "remote".to_owned();
                } else if real_models {
                    let effective = if self.detection_models.sam {
                        self.sam.as_ref().map(|model| model.backend())
                    } else if self.detection_models.rt_full {
                        self.full_rt.as_ref().map(|model| model.selection().accelerator)
                    } else if self.detection_models.rt_small {
                        self.balloons.held.as_ref().map(|model| model.selection().accelerator)
                    } else { None };
                    if let Some(effective) = effective {
                        at.provider = format!("{:?}", effective).to_lowercase();
                    }
                }
            }
            // **Text groups.** The regions are the grouping's cleaning groups:
            // one region per group, seeded by the group's exact glyph pixels,
            // flagged against the page's median *block* rather than a median
            // polluted by per-glyph boxes. Candidates (lettering no box
            // claimed) are held with their lettering until every segment that
            // sees their rows has run, and never cleaned here.
            //
            // **The second source of boxes.** Outside a balloon the balloon
            // detector sees text the text detector does not emit a box for -
            // narration boxes, caption lines, a chapter title strip - and on an
            // Ogkalu-only run its boxes are the only ones. The grouping names
            // exactly the text boxes with no mask pixels under them and gives
            // each its bounded ink estimate, taken once. The gate below asks
            // such a region exactly what it asks every other region.
            //
            // The balloon boxes and text regions are both in this bounded
            // crop's coordinates. The detection overlap makes ordinary boxes
            // whole in one crop. An adopted box that reaches a crop edge can
            // be taller than that overlap. A confirmed continuation replaces
            // its normal result with one held review row.
            let median = grouping.block_median_area();
            let mut regions: Vec<_> = grouping.cleaning().map(|group| grouping.region(group, median)).collect();
            let adopted: Vec<_> = grouping.detector_only()
                .map(|group| grouping.detector_only_region(group, median))
                .collect();
            let scale = detection.segmentation.proxy_scale();
            grouping.paint_estimates(&mut detection.segmentation);
            // After the estimates are painted, so a candidate's paper is read
            // against the segmentation its segment's regions are judged on.
            held.extend(grouping.candidates().map(|group| {
                let inside = candidate_in_bubble(&grouping, group, &bounded.raster, &detection.segmentation, &balloons);
                HeldCandidate::of(&grouping, group, segment.index, (crop_x, crop_y), scale, inside)
            }));
            let adopted_rects: Vec<Rect> = adopted.iter().map(|region| region.masking).collect();
            // Each current box consumes at most one prior. Every other prior
            // keeps its normal result, or its already confirmed review row.
            let mut matched_priors = Vec::with_capacity(adopted_rects.len());
            let mut withdrawn = Vec::new();
            for rect in &adopted_rects {
                let in_strip = Rect::new(rect.x + crop_x, rect.y + crop_y, rect.w, rect.h);
                let prior = awaiting_continuation.iter()
                    .position(|(whole, _, _)| clipped_continuation(whole, &in_strip, crop_y))
                    .map(|position| awaiting_continuation.remove(position));
                if let Some((_, _, Some(position))) = prior {
                    withdrawn.push(position);
                }
                matched_priors.push(prior.map(|(whole, region_index, _)| (whole, region_index)));
            }
            // Remove later results first so earlier outcome positions remain valid.
            withdrawn.sort_unstable_by_key(|position| std::cmp::Reverse(*position));
            for position in withdrawn {
                at.outcome.regions.remove(position);
            }
            for (whole, region_index, outcome_position) in awaiting_continuation {
                if region_index.is_some() && outcome_position.is_none() {
                    at.outcome.regions.push(RegionOutcome::Untouched {
                        bbox: Rect::new(whole.x - page_x, whole.y - page_y, whole.w, whole.h),
                        reason: "review.reason.gateSkippedLowConfidence".into(),
                    });
                }
            }
            regions.extend(adopted);
            // Sorted rather than appended, because `region_id` names a region
            // by its index in this list: appending would give the adopted
            // regions the last ids on the page instead of their own place in
            // reading order, and two runs have to agree on the order.
            cleaner_core::detect::sort_regions(&mut regions);
            // Rule 6's shape at the segment scale: the crop and its
            // segmentation live as long as this segment's work does, and
            // longer only while a region it saw waits on a later segment.
            let seen = Arc::new(SeenSegment {
                index: segment.index,
                crop: bounded.raster,
                origin: (crop_x, crop_y),
                page_origin: (page_x, page_y),
                segmentation: detection.segmentation,
                balloons,
                grouping,
                evidence: segment_evidence,
            });
            let crop_height = i64::from(seen.crop.height);

            // Regions are owned by region evidence alone. A candidate merged
            // into a region's global box could move its owner to a segment
            // that only listed that text, and then no segment would clean it.
            let found: Vec<DetectedInSegment> = regions
                .iter()
                .map(|region| {
                    DetectedInSegment::new(
                        segment.index,
                        Rect::new(region.masking.x + crop_x, region.masking.y + crop_y,
                            region.masking.w, region.masking.h),
                        region.members.iter().map(|m| m.confidence).fold(0.0, f32::max),
                    )
                })
                .collect();
            // Every region's lettering claims candidate lettering, whichever
            // segment cleans it: text one segment boxed is not listed again
            // because another saw it unboxed.
            claims.extend(regions.iter().map(|region| Claim::of(region, segment.index, (crop_x, crop_y))));
            // The evidence: this segment's boxes, the previous segment's, and
            // every region still waiting on its owner.
            let mut evidence = previous.clone();
            evidence.extend(pending.iter().map(|deferred| deferred.found).filter(|found| !previous.contains(found)));
            evidence.extend(found.iter().copied());
            let globals = merge_global(&evidence, &cuts, context.segments);

            for (region, found) in regions.iter().zip(&found) {
                let strip_rect = Rect::new(region.masking.x + crop_x, region.masking.y + crop_y,
                    region.masking.w, region.masking.h);
                if let Some(adopted_index) = adopted_rects.iter()
                    .position(|rect| *rect == region.masking) {
                    if let Some((prior, prior_index)) = matched_priors[adopted_index] {
                        let whole = rect_hull(prior, strip_rect);
                        if region.masking.bottom() == crop_height
                            && Some(segment.index) != last_segment {
                            clipped_adopted.push((whole, prior_index, None));
                        } else {
                            at.outcome.regions.push(RegionOutcome::Untouched {
                                bbox: Rect::new(whole.x - page_x, whole.y - page_y, whole.w, whole.h),
                                reason: "review.reason.gateSkippedLowConfidence".into(),
                            });
                            if prior_index.is_none() { at.index += 1; }
                        }
                        continue;
                    }
                }
                let clipped_position = if adopted_rects.contains(&region.masking)
                    && region.masking.bottom() == crop_height
                        && Some(segment.index) != last_segment {
                    clipped_adopted.push((strip_rect, None, None));
                    Some(clipped_adopted.len() - 1)
                } else { None };
                let owner = global_of(&globals, strip_rect).map(|global| global.owner);
                if owner.is_some_and(|owner| owner != segment.index) {
                    // Some other segment's work. A box in this segment's
                    // detection overlap is whole here *and* whole there, and
                    // rule 3's whole point is that exactly one of them cleans
                    // it. A later segment's is kept until that segment has
                    // run: should its detector not find it, it is written from
                    // this crop rather than lost at the cut.
                    if let Some(owner) = owner.filter(|owner| *owner > segment.index) {
                        pending.push(Deferred { seen: Arc::clone(&seen), region: region.clone(), rect: strip_rect,
                            found: *found, owner });
                    }
                    continue;
                }
                if adopted_rects.contains(&region.masking) && region.masking.y == 0
                    && Some(segment.index) != first_segment {
                    let region_index = at.index;
                    at.outcome.regions.push(RegionOutcome::Untouched {
                        bbox: Rect::new(strip_rect.x - page_x, strip_rect.y - page_y,
                            strip_rect.w, strip_rect.h),
                        reason: "review.reason.gateSkippedLowConfidence".into(),
                    });
                    at.index += 1;
                    if let Some(position) = clipped_position {
                        clipped_adopted[position].1 = Some(region_index);
                        clipped_adopted[position].2 = Some(at.outcome.regions.len() - 1);
                    }
                    continue;
                }
                if let Some(position) = clipped_position {
                    clipped_adopted[position].1 = Some(at.index);
                }
                if self.emit_region(&mut at, &seen, region, strip_rect)? {
                    if let Some(position) = clipped_position {
                        clipped_adopted[position].2 = Some(at.outcome.regions.len() - 1);
                    }
                }
            }
            // Deferred regions whose owner has now run. One the owner found
            // is its work; one it did not is written from the crop that saw
            // it. Nothing is lost at a cut, and nothing is written twice.
            let mut waiting = Vec::with_capacity(pending.len());
            let mut missed = Vec::new();
            for mut deferred in std::mem::take(&mut pending) {
                match settle(&globals, &deferred, segment.index) {
                    Settled::Waiting(owner) => {
                        deferred.owner = owner;
                        waiting.push(deferred);
                    }
                    Settled::Found => {}
                    Settled::Missed => missed.push(deferred),
                }
            }
            for deferred in one_per_region(missed, &cuts, context.segments) {
                self.emit_region(&mut at, &deferred.seen, &deferred.region, deferred.rect)?;
            }
            pending = waiting;
            previous = found;
        }
        for (whole, region_index, outcome_position) in clipped_adopted {
            if region_index.is_some() && outcome_position.is_none() {
                at.outcome.regions.push(RegionOutcome::Untouched {
                    bbox: Rect::new(whole.x - page_x, whole.y - page_y, whole.w, whole.h),
                    reason: "review.reason.gateSkippedLowConfidence".into(),
                });
            }
        }
        // A deferred region whose owner is on this page and never ran (an
        // empty window) is written now; one the next page owns waits for it.
        let (later, due): (Vec<Deferred>, Vec<Deferred>) = pending.into_iter()
            .partition(|deferred| last_segment.is_some_and(|last| deferred.owner > last));
        for deferred in one_per_region(due, &cuts, context.segments) {
            self.emit_region(&mut at, &deferred.seen, &deferred.region, deferred.rect)?;
        }
        // Candidates last, once every region of the page has claimed its
        // lettering. What starts on the next page's rows is the next page's.
        let (next, now): (Vec<HeldCandidate>, Vec<HeldCandidate>) =
            held.into_iter().partition(|candidate| candidate.rect.y >= page_end);
        list_candidates(now, &claims, &mut listed, context.segments, &mut at.outcome.regions, (page_x, page_y));
        claims.retain(|claim| claim.rect.bottom() > page_end);
        listed.retain(|entry| entry.rect.bottom() > page_end);
        self.carried = Carried { found: previous, deferred: later, candidates: next, claims, listed };
        self.previous_placement = Some(context.placement);
        // The window is context for the models; the rows are the area's. A
        // row is the area's when its box touches it.
        if let Some((area, _)) = area {
            at.outcome.regions.retain(|region| {
                let bbox = match region {
                    RegionOutcome::Cleaned(patch, _) => patch.mask.bounds,
                    RegionOutcome::Untouched { bbox, .. }
                    | RegionOutcome::Candidate { bbox, .. }
                    | RegionOutcome::Declined { bbox, .. } => *bbox,
                    RegionOutcome::Detected(found) => found.record.bbox,
                };
                let touched = shared(bbox, area);
                touched.w > 0 && touched.h > 0
            });
        }
        Ok(at.outcome)
    }

    /// Judge, fit and write one region seen in `seen`'s crop as the page's
    /// next row: a detection, a patch, or an untouched row with its reason.
    ///
    /// The region takes the page's next index whether or not a row is written,
    /// and the answer is whether one was. `seen` is the segment that saw the
    /// region, which is this page's current segment, or an earlier one whose
    /// deferred region its owner did not find.
    fn emit_region(
        &mut self,
        at: &mut PageEmission,
        seen: &SeenSegment,
        region: &cleaner_core::detect::Region,
        strip_rect: Rect,
    ) -> Result<bool, String> {
        let started = Instant::now();
        let region_index = at.index;
        let id = region_id(at.page_id, region_index);
        let order = region_index as u32;
        at.index += 1;
        let (page_x, page_y) = at.origin;
        let (crop_x, crop_y) = seen.origin;
        let crop = &seen.crop;
        let scale = seen.segmentation.proxy_scale();

        // Everything the manifest records is in **page** coordinates,
        // so a bbox reported to the seam means the same thing whether
        // the page was one segment or six.
        let on_page = Rect::new(strip_rect.x - page_x, strip_rect.y - page_y,
            strip_rect.w, strip_rect.h);

        // **Inside or outside, once.** The one answer
        // ([`cleaner_core::balloon::in_bubble`]) the gate, the seed, the engine
        // pick and the stored detection all read, whatever the text policy:
        // under All text the gate never runs, and the answer is still this.
        // `detected` is only the detector's grade, for the gate's reader.
        let detected = cleaner_core::balloon::detected(region.masking, &seen.balloons);
        let inside = cleaner_core::balloon::in_bubble(
            crop, &seen.segmentation, region.text_bounds(), region.masking, &seen.balloons);
        let verdict = self.judge_region(crop, &seen.segmentation, region, detected, inside)?;
        if !self.selection.allows(&verdict) {
            let reason = verdict.reason_key().unwrap_or(match verdict {
                Verdict::OptedIn => "review.reason.outsideLanguageUnverified",
                _ => "review.reason.languageSkipped",
            });
            at.outcome.regions.push(RegionOutcome::Untouched {
                bbox: on_page,
                reason: reason.to_owned(),
            });
            return Ok(true);
        }

        // A text group seeds from its own glyph pixels, never from
        // whatever else of the segmentation its box reaches over.
        let held = region.group.as_ref().ok_or("text region without its group")?;
        let seed = held.lettering.clone();
        if seed.is_empty() {
            // A text box with no lettering and no ink under it is
            // listed with its reason rather than dropped.
            at.outcome.regions.push(RegionOutcome::Untouched {
                bbox: on_page,
                reason: held.group.reasons.iter().min()
                    .map_or(text_groups::ReviewReason::MaskMissingUnderTextBox.key(), |r| r.key())
                    .to_owned(),
            });
            return Ok(true);
        }
        // The group's record: identity, provenance and its reasons.
        // The size flag keeps precedence as the review key because the
        // library view restores it from the key; a group reason is
        // persisted as the key otherwise, and always in the record.
        let (grouping, segment_evidence) = seen.evidence_on(at.origin)?;
        let group_record = Some(grouping.record(&held.group, Some(&segment_evidence)));
        let review_state = region.flagged_large.then(|| "review.reason.unusuallyLarge".to_owned())
            .or_else(|| group_record.as_ref().and_then(|record| record.review_key()).map(str::to_owned));

        // **Rule 4.** Every term native, and the clamp is rule 1's. The box
        // is widened by the requested padding so the output mask and the
        // background fit both have enough surrounding context.
        let window = anchored_engine_window(at.context, padded(strip_rect, self.mask_padding), scale, at.engine_context);
        // Detection overlap is forward-only for deterministic ownership,
        // but fitting and rendering need context on both sides. Read that
        // bounded engine window separately; if a neighbour changed after
        // survey, retain the valid detection crop instead of failing the page.
        let engine_read = at.context.read_window(window, at.page).ok();
        let (engine_crop, engine_x, engine_y) = engine_read.as_ref()
            .map(|read| (&read.raster, read.origin.0, read.origin.1))
            .unwrap_or((crop, crop_x, crop_y));
        let seed = translated(seed, crop_x - engine_x, crop_y - engine_y);
        let x0 = (window.rect.x - engine_x).max(0);
        let y0 = (window.rect.y - engine_y).max(0);
        let x1 = (window.rect.right() - engine_x).min(engine_crop.width as i64);
        let y1 = (window.rect.bottom() - engine_y).min(engine_crop.height as i64);
        let bounds = Rect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32);
        if bounds.w == 0 || bounds.h == 0 {
            return Ok(false);
        }
        let edges = EdgeMap::sobel(engine_crop);
        // Padding owns the total growth from SAM, including outside balloons.
        // The background fit still chooses a route, but its automatic mask,
        // outline and model-hole growth must not become a second padding.
        let fitted = fit::fit_with_padding(
            engine_crop, &seed, scale, at.noise, &edges, bounds, self.mask_padding);
        let unpadded = (self.mask_padding > 0).then(|| (seed.clone(), seed.clone()));

        // **The pressure ladder, between regions.** The last region's
        // buffers are gone and this one's model tensors do not exist
        // yet, so a step taken here gives back a session rather than
        // interrupting an inference.
        if let Some(step) = self.under_pressure() {
            at.outcome.pressure.push(step);
        }
        // And the voluntary half of the same moment: a session nobody
        // is using, or one somebody asked for back. See
        // [`Pipeline::reap`].
        self.reap();

        // Which kind of text this is: the same answer the gate was
        // handed above, and the only thing the two picks are told
        // apart by.
        let pick = self.picks.map(|picks| picks.for_region(inside));
        let solid_color = if pick == Some(EnginePick::Solid) {
            self.bubble_color
        } else {
            None
        };

        // **Detect stops here.** The region is found, gated and fitted,
        // and the ladder is the one step it has not taken: the fit's
        // masks and its own answers go to the manifest instead, in page
        // pixels, and a later clean starts from exactly this.
        if at.detect_only {
            let script = script_code(&verdict);
            let (dx, dy) = (engine_x - page_x, engine_y - page_y);
            // The ring's median is 16-bit luma, and a balloon's fill is
            // paper: one grey, in 8-bit.
            let tone = ((u32::from(fitted.ring.median) + 128) / 257).min(255) as u8;
            at.outcome.regions.push(RegionOutcome::Detected(Box::new(LoadedDetection {
                record: DetectedRegion {
                    // Both minted by the walk, which holds the manifest.
                    id: String::new(),
                    source_idx: 0,
                    bbox: on_page,
                    mask_ref: String::new(),
                    inside,
                    balloon_color: inside.then_some([tone; 3]),
                    script,
                    pick: stored_pick(fitted.route, at.ceiling, pick).to_owned(),
                    detector: if self.remote_detected() { "cloud" } else { "local" }.to_owned(),
                    created: now(),
                    mask_sha256: String::new(),
                    order,
                    review_state,
                    fit: Some(DetectionFit {
                        route: fitted.route,
                        thickness: fitted.thickness,
                        best_deviation: fitted.best_deviation,
                        scale,
                    }),
                    group: group_record,
                    padding_from_seed: true,
                    padding_px: self.mask_padding,
                },
                mask: translated(fitted.mask, dx, dy),
                ink: translated(fitted.ink, dx, dy),
                evidence: Some(segment_evidence),
                base: unpadded.map(|(mask, ink)| (translated(mask, dx, dy), translated(ink, dx, dy))),
            })));
            return Ok(true);
        }

        let attempt =
            match clean_region_with_color(&mut self.rung2, engine_crop, &fitted, at.ceiling, pick, at.noise, solid_color) {
                Ok(attempt) => attempt,
                // **The one thing the ladder can fail on is rung 2's
                // own run call.** Every rung below it is arithmetic
                // over the page's pixels and cannot fault, every rung
                // above it is refused before it is reached, and every
                // other outcome - a refusal, a quality decline, an
                // empty ladder - comes back as `Declined`. So an `Err`
                // here names LaMa without having to be told.
                // [`render_rung`] has already poisoned the session;
                // what is left is to say which model it was and where
                // it was running, which is a question only this side
                // can answer.
                Err(detail) => {
                    self.fault = Some(EngineFault {
                        model_key: accel::LAMA.label_key,
                        accel_key: self.rung2.accel_key(),
                        detail: detail.clone(),
                    });
                    return Err(detail);
                }
            };
        let (made, verdict) = match attempt {
            Attempt::Declined(reason) => {
                // §6: a declined region is **left exactly as it was**
                // and listed in review with its reason. Not
                // half-cleaned, and not cleaned by a rung that already
                // failed - leaving text is recoverable and a bad fill
                // is not.
                at.outcome.regions.push(RegionOutcome::Declined {
                    bbox: on_page,
                    reason: reason.to_owned(),
                    lettering: Some((translated(seed, engine_x - page_x, engine_y - page_y), scale)),
                });
                return Ok(true);
            }
            Attempt::Cleaned(made, verdict) => (*made, verdict),
        };
        let mask = translated(made.mask, engine_x - page_x, engine_y - page_y);
        let ink = translated(made.ink, engine_x - page_x, engine_y - page_y);

        // Rule 4's term is the window's, and rung 2 reports its own -
        // the tiles it ran are what actually met an edge. Either one
        // replicating is a replicated patch.
        let pad = if made.pad == strip::EdgePad::None { window.pad } else { made.pad };
        let mask_sha256 = mask_digest(&mask);
        let mut snapshot = params_snapshot(
            made.engine, &fitted, started.elapsed(), pad, made.tiles, &verdict,
        );
        if let Some(record) = group_record.as_ref() {
            snapshot["group"] = record.snapshot();
            if !at.outcome.evidence.iter().any(|blob| blob.sha256 == segment_evidence.sha256) {
                at.outcome.evidence.push(segment_evidence);
            }
        }
        at.outcome.detected_boxes.insert(id.clone(), on_page);
        at.outcome.regions.push(RegionOutcome::Cleaned(
            Box::new(Patch {
                id,
                mask,
                ink,
                pixels: made.pixels,
                order,
                visible: true,
                provenance: Provenance {
                    engine: made.engine,
                    engine_version: self.engine_version.into(),
                    model_sha256: made.model_sha256,
                    // A rung with a session of its own names where that
                    // session landed; rung 0 has none, and the
                    // run's provider is the only one there is to give.
                    execution_provider: made
                        .provider
                        .unwrap_or_else(|| at.provider.clone()),
                    params_snapshot: snapshot,
                    mask_sha256,
                    source_sha256: at.source_sha256.to_owned(),
                    cloud: None,
                    created: now(),
                },
            }),
            review_state,
        ));
        Ok(true)
    }

    /// Clean a page's stored detections: Clean, and the half of Auto that finds
    /// detections waiting.
    ///
    /// **The region is rebuilt, not re-found.** The engine window is placed
    /// from the stored box and the stored segmentation scale, which is where
    /// [`Pipeline::analyse_page`] placed it; the stored mask goes through the
    /// manual fit, which measures the paper ring around exactly that mask and
    /// adds nothing to it; and the lettering, the route and the growth the
    /// candidate search chose are put back from the record. That is the whole
    /// of the fit a clean could not re-derive without running the search again,
    /// and the search needs the segmentation, which needs the detector.
    ///
    /// Everything after the rebuild is the one-pass run's: the pressure step
    /// between regions, the pick moving the start, [`clean_region_with_color`]
    /// with its ceiling and its quality loop, and the same patch record. A rung
    /// 2 fault fails the page and keeps nothing, as it does there.
    fn clean_stored(
        &mut self,
        bytes: &[u8],
        ceiling: Engine,
        context: &PageContext,
        detections: &[LoadedDetection],
    ) -> Result<DetectionsOutcome, String> {
        self.fault = None;
        let page = cleaner_core::image::orientation::from_bytes(bytes).raster(&decode(bytes).map_err(|e| e.to_string())?);
        cleaner_core::image::color::validate_editing(&page).map_err(|e|e.to_string())?;
        let source_sha256 = sha256_hex(bytes);
        self.reap();
        let noise = fit::page_noise_sigma(&page);
        let engine_context = if rung(ceiling) >= rung(Engine::Lama) {
            EngineContext::Local
        } else {
            EngineContext::None
        };
        let (page_x, page_y) = context.origin();
        let mut outcome = DetectionsOutcome::default();

        for detection in detections {
            let record = &detection.record;
            let started = Instant::now();
            let strip_rect = Rect::new(record.bbox.x + page_x, record.bbox.y + page_y,
                record.bbox.w, record.bbox.h);
            let scale = record.fit.map_or(1.0, |fit| fit.scale);
            // Widened by the stored padding, as Detect widened its own window.
            let window = anchored_engine_window(context, padded(strip_rect, record.padding_px), scale, engine_context);
            // A neighbour that changed since the survey costs the window its
            // context, as it does in `analyse_page`, and not the region.
            let engine_read = context.read_window(window, &page).ok();
            let (engine_crop, engine_x, engine_y) = engine_read.as_ref()
                .map(|read| (&read.raster, read.origin.0, read.origin.1))
                .unwrap_or((&page, page_x, page_y));
            let mask = translated(detection.mask.clone(), page_x - engine_x, page_y - engine_y);
            let ink = translated(detection.ink.clone(), page_x - engine_x, page_y - engine_y);
            let x0 = (window.rect.x - engine_x).max(0);
            let y0 = (window.rect.y - engine_y).max(0);
            let x1 = (window.rect.right() - engine_x).min(engine_crop.width as i64);
            let y1 = (window.rect.bottom() - engine_y).min(engine_crop.height as i64);
            let bounds = Rect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32);
            if mask.is_empty() || bounds.w == 0 || bounds.h == 0 {
                outcome.regions.push((record.id.clone(), RegionOutcome::Untouched {
                    bbox: record.bbox,
                    reason: "decline.reason.rungUnavailable".into(),
                }));
                continue;
            }
            let edges = EdgeMap::sobel(engine_crop);
            let mut fitted = fit::fit_within(engine_crop, &mask, 1.0, noise, &edges, true, bounds);
            fitted.ink = ink;
            if let Some(stored) = record.fit {
                fitted.route = stored.route;
                fitted.thickness = stored.thickness;
                fitted.best_deviation = stored.best_deviation;
            }

            if let Some(step) = self.under_pressure() {
                outcome.pressure.push(step);
            }
            self.reap();

            // The run's picks are the clean's own: Text cleanup shows them
            // for Clean and for Detect & clean, so a region detected earlier
            // starts where a region found in this run would, by whether it is
            // inside a balloon. A pass with no picks of its own (a cloud
            // clean's LaMa or flat-colour pass, a Layers row) keeps the saved
            // pick, which is the rung the detecting run would have started on:
            // handed back, it starts the ladder there again under the same
            // ceiling (`start_rung` answers a pick at or under the ceiling
            // with the pick itself).
            let pick = self.picks.map(|picks| picks.for_region(record.inside))
                .or_else(|| parse_pick(Some(&record.pick)));
            // The balloon's own fill, measured when it was detected, before the
            // run's colour setting: the interface always sends one (white by
            // default), so the setting first would paint every grey balloon
            // white. A detection outside a balloon stored none and takes the
            // run's colour, as one pass would.
            let solid_color = if pick == Some(EnginePick::Solid) {
                record.balloon_color.or(self.bubble_color)
            } else {
                None
            };
            let attempt =
                match clean_region_with_color(&mut self.rung2, engine_crop, &fitted, ceiling, pick, noise, solid_color) {
                    Ok(attempt) => attempt,
                    // Rung 2's own run call, as in `analyse_page`.
                    Err(detail) => {
                        self.fault = Some(EngineFault {
                            model_key: accel::LAMA.label_key,
                            accel_key: self.rung2.accel_key(),
                            detail: detail.clone(),
                        });
                        return Err(detail);
                    }
                };
            let (made, verdict) = match attempt {
                Attempt::Declined(reason) => {
                    outcome.regions.push((record.id.clone(), RegionOutcome::Untouched {
                        bbox: record.bbox,
                        reason: reason.to_owned(),
                    }));
                    continue;
                }
                // A solid fill skips the quality metric because it is a colour
                // the user chose. Local first asks something else of it: is it
                // good enough that the cloud is not needed? So there the
                // metric decides, and a fill it refuses goes to the cloud.
                Attempt::Cleaned(_, verdict) if self.gate_solid_fill && verdict.cause().is_some() => {
                    let reason = verdict.cause().map_or("decline.reason.rungUnavailable", decline_key);
                    outcome.regions.push((record.id.clone(), RegionOutcome::Untouched {
                        bbox: record.bbox,
                        reason: reason.to_owned(),
                    }));
                    continue;
                }
                Attempt::Cleaned(made, verdict) => (*made, verdict),
            };
            let mask = translated(made.mask, engine_x - page_x, engine_y - page_y);
            let ink = translated(made.ink, engine_x - page_x, engine_y - page_y);
            let pad = if made.pad == strip::EdgePad::None { window.pad } else { made.pad };
            let mask_sha256 = mask_digest(&mask);
            let mut snapshot = params_snapshot(
                made.engine,
                &fitted,
                started.elapsed(),
                pad,
                made.tiles,
                &verdict,
            );
            // The detection's group travels onto its patch.
            if let Some(group) = record.group.as_ref() {
                snapshot["group"] = group.snapshot();
            }
            outcome.regions.push((record.id.clone(), RegionOutcome::Cleaned(
                Box::new(Patch {
                    id: record.id.clone(),
                    mask,
                    ink,
                    pixels: made.pixels,
                    order: record.order,
                    visible: true,
                    provenance: Provenance {
                        engine: made.engine,
                        engine_version: self.engine_version.into(),
                        model_sha256: made.model_sha256,
                        // No detector ran, so there is no run provider to stand
                        // in for a rung without a session: rung 0 is
                        // arithmetic on this machine's processor.
                        execution_provider: made.provider.unwrap_or_else(|| "cpu".to_owned()),
                        params_snapshot: snapshot,
                        mask_sha256,
                        source_sha256: source_sha256.clone(),
                        cloud: None,
                        created: now(),
                    },
                }),
                record.review_state.clone(),
            )));
        }
        Ok(outcome)
    }
}

/// The name a stored detection records for the rung its region starts on.
///
/// The **effective** start - [`start_rung`] over the fit's route, the run's
/// ceiling and the user's pick - rather than the pick alone, so a cloud clean
/// asking "is this a fill region" reads the answer the ladder would act on.
/// `solid` survives as itself, because it is a fill with a colour and the
/// colour is the pick's. A region no rung under this ceiling can take keeps
/// the rung its route needs, and a clean under the same ceiling declines it
/// exactly as one pass would.
fn stored_pick(route: Route, ceiling: Engine, pick: Option<EnginePick>) -> &'static str {
    match start_rung(route, ceiling, pick) {
        Some(Engine::Fill) if pick == Some(EnginePick::Solid) => "solid",
        Some(Engine::Fill) => "fill",
        Some(_) => "lama",
        None => pick.map_or("lama", pick_name),
    }
}

/// The gate's script name as the seam's language code. Han is written `zh`:
/// the identifier cannot tell kanji from hanzi, and a run that cleaned it did
/// so because one of the two was selected.
fn script_code(verdict: &Verdict) -> Option<String> {
    let Verdict::Clean { script } = verdict else { return None };
    let script = script.strip_suffix("-dn").unwrap_or(script);
    Some(match script {
        "Japanese" | "Japanese_vert" | "Japanese_ocr" => "ja",
        "HanS" | "HanS_vert" | "HanT" | "HanT_vert" | "Han_ocr" => "zh",
        "Hangul" | "Hangul_vert" | "Hangul_ocr" => "ko",
        _ => return None,
    }.to_owned())
}

/* ------------------------------------------------------------------ */
/* Try, escalate, decline                                              */
/* ------------------------------------------------------------------ */

/// What the ladder answered for one region.
pub(crate) enum Attempt {
    /// A rung produced pixels and [`quality::assess`] did not decline them.
    Cleaned(Box<Made>, quality::Assessment),
    /// No rung the region could reach produced pixels the metric would accept.
    /// The key is the **last** refusal's, which is the highest rung's: that is
    /// the reason the region is untouched, where the rungs below it are only
    /// the reason it got that far.
    Declined(&'static str),
}

/// One rung's output, and everything about how it was made that the provenance
/// record wants.
pub(crate) struct Made {
    pub(crate) engine: Engine,
    pub(crate) mask: Mask,
    /// [`Patch::ink`]: the lettering, as the rung saw it. `Fitted::ink` for
    /// every rung - rung 2's hole is cut to exactly that - and the stroke
    /// itself for a hand tool.
    pub(crate) ink: Mask,
    pub(crate) pixels: Raster,
    /// The rung's own execution provider, where the rung has a session.
    pub(crate) provider: Option<String>,
    pub(crate) model_sha256: Option<String>,
    pub(crate) pad: strip::EdgePad,
    /// How many model runs the region cost. `None` below rung 2, where there
    /// are no tiles because there is no model - not zero, which would read as a
    /// model that ran nothing.
    pub(crate) tiles: Option<u32>,
}

/// What one rung answered, before it was scored.
pub(crate) enum Rendered {
    Made(Box<Made>),
    /// The rung refused the region before it ran, under its own key.
    Refused(&'static str),
}

/// Try the routed rung, score it, and escalate until something passes or
/// the ladder runs out.
///
/// The rule is one sentence and a loop: "a region is declined when **no
/// engine's output** passes the uniform quality metric". So each rung's
/// pixels are scored by
/// [`quality::assess`] - the same function for every rung, which is what
/// makes a review row saying *declined* mean the same thing whichever rung
/// produced them - and a declining rung hands the region to the next one
/// [`ladder_from`] permits rather than being written to disk.
///
/// **A decline is not a fault.** A rung that refuses before it runs - a box
/// past 2048 px, a mode a model cannot write into - is a decision with a key
/// of its own, and the escalation continues past it. A rung the model would
/// not *answer* is a fault, and it comes back as `Err`, which fails the page
/// and leaves it queued (the two are kept apart).
///
/// **An unmeasurable region is accepted, not declined.** [`quality`]'s own
/// argument: a declined region leaves text on the page, and the
/// justification for accepting that cost is an argument for declining when
/// the metric *fails*, not when it is absent.
///
/// **Where it starts is the user's**, through [`start_rung`]: `pick` is the
/// answer they gave for this kind of text, and `None` is the route's own rung.
/// It moves the start and nothing else - the list this loop walks, and every
/// rung above the start in it, are the same either way.
pub(crate) fn clean_region(
    rung2: &mut Rung2,
    crop: &Raster,
    fitted: &fit::Fitted,
    ceiling: Engine,
    pick: Option<EnginePick>,
    noise: f32,
) -> Result<Attempt, String> {
    clean_region_with_color(rung2, crop, fitted, ceiling, pick, noise, None)
}

pub(crate) fn clean_region_with_color(
    rung2: &mut Rung2,
    crop: &Raster,
    fitted: &fit::Fitted,
    ceiling: Engine,
    pick: Option<EnginePick>,
    noise: f32,
    solid_color: Option<[u8; 3]>,
) -> Result<Attempt, String> {
    cleaner_core::image::color::validate_editing(crop).map_err(|e| e.to_string())?;
    let Some(start) = start_rung(fitted.route, ceiling, pick) else {
        return Ok(Attempt::Declined("decline.reason.rungUnavailable"));
    };
    // The answer if the ladder is empty, which is the same statement the
    // empty list makes: the rung this region needs is not one this run has.
    let mut refused = "decline.reason.rungUnavailable";

    for engine in ladder_from(start, ceiling, crop) {
        let made = match render_rung_with_color(rung2, engine, crop, fitted, solid_color)? {
            Rendered::Refused(key) => {
                refused = key;
                continue;
            }
            Rendered::Made(made) => made,
        };
        // The **applied** mask and the pixels that would ship, which is
        // exactly the pair `assess` asks for: the surround has to begin
        // outside everything the edit wrote.
        let verdict = quality::assess(crop, &made.mask, &made.pixels, noise);
        // A solid colour is an explicit paint choice, not a reconstruction
        // proposal for the quality metric to replace with another engine.
        // Keep the verdict for provenance, but honour the chosen colour even
        // when it deliberately differs from the surrounding paper.
        if solid_color.is_some() && engine == Engine::Fill {
            return Ok(Attempt::Cleaned(made, verdict));
        }
        if let Some(cause) = verdict.cause() {
            refused = decline_key(cause);
            // Rule 6 at the rung scale: this rung's buffers go before the
            // next rung's are built, so escalating costs one patch and not
            // one per rung tried.
            drop(made);
            continue;
        }
        return Ok(Attempt::Cleaned(made, verdict));
    }
    Ok(Attempt::Declined(refused))
}

/// One rung, run once.
///
/// `crop` is the segment's own pixels rather than the decode window's, and
/// that is safe in this build for the reason [`PageContext::to_crop`] gives
/// from the other side: a crop never spans a join, so the window's clamp and
/// the crop's edge fall in the same place, and rung 2's "page edge" is rule
/// 4's page edge. A reader that could hold two pages across a verified join
/// would have to hand this the window itself.
pub(crate) fn render_rung(
    rung2: &mut Rung2,
    engine: Engine,
    crop: &Raster,
    fitted: &fit::Fitted,
) -> Result<Rendered, String> {
    render_rung_with_color(rung2, engine, crop, fitted, None)
}

pub(crate) fn render_rung_with_color(
    rung2: &mut Rung2,
    engine: Engine,
    crop: &Raster,
    fitted: &fit::Fitted,
    solid_color: Option<[u8; 3]>,
) -> Result<Rendered, String> {
    let plain = |mask: Mask, pixels: Raster| {
        Rendered::Made(Box::new(Made {
            engine,
            mask,
            ink: fitted.ink.clone(),
            pixels,
            provider: None,
            // Arithmetic over the page's own pixels. There is no model, so
            // there is no digest - `None` is the true answer, not a missing
            // one.
            model_sha256: None,
            pad: strip::EdgePad::None,
            tiles: None,
        }))
    };
    match engine {
        Engine::Lama => {
            let Some(held) = rung2.held()? else {
                return Ok(Rendered::Refused("decline.reason.rungUnavailable"));
            };
            // Bound rather than matched on directly, because the arms below
            // need the session again and a scrutinee's borrow of it would still
            // be alive there.
            let rendered = held.inpainter.render(crop, fitted);
            match rendered {
                Ok(rendered) => Ok(Rendered::Made(Box::new(Made {
                    engine,
                    mask: rendered.mask,
                    ink: lama::model_hole(fitted),
                    pixels: rendered.pixels,
                    provider: Some(held.provider.clone()),
                    model_sha256: held.model_sha256.clone(),
                    pad: rendered.pad,
                    tiles: Some(rendered.tiles),
                }))),
                Err(lama::Error::Declined(declined)) => {
                    Ok(Rendered::Refused(declined.reason_key()))
                }
                // **The session does not survive this, so it is not kept.** A
                // decline above is the model answering; this is the model
                // failing to answer, and on Windows the cause is usually one
                // the session cannot come back from - the driver watchdog reset
                // the device mid-inference, or the device ran out of memory.
                // Once a Direct3D device is lost every later run on that
                // session fails too, so a run that held on to it would carry
                // one fault into every remaining page, every later run and
                // every hand edit until the idle grace expired. Poisoning hands
                // it to the disposal path that already exists - see
                // [`Faulted::poison_session`] - and the next region that wants
                // a rung 2 opens a fresh one.
                //
                // It is done **here** rather than at the two call sites because
                // this is the only place that knows the failure was a run and
                // not a decline. Naming the model for the notice is the callers'
                // half of it, in [`Cleaner::clean_page`] and in
                // [`crate::region`].
                Err(lama::Error::Run(fault)) => {
                    held.inpainter.poison_session();
                    Err(fault)
                }
            }
        }
        // Rung 3a and rung 4, neither of which an automatic run can reach.
        // `ladder_from` never names either, so this arm is unreachable - and
        // it is a **refusal** rather than a fill for exactly that reason. The
        // wildcard that used to stand here would have answered a `Flux` or a
        // `Cloud` with rung 0's pixels wearing rung 3a's or rung 4's name, and
        // a patch whose provenance says `flux` over a planar fill is a lie in
        // the manifest that no later reader could detect.
        Engine::Flux | Engine::Cloud => {
            Ok(Rendered::Refused("decline.reason.rungUnavailable"))
        }
        // **A painted stroke never arrives here, and if it does the answer is a
        // refusal rather than pixels.** `region::edit` intercepts paint and
        // clone before the fit runs, because neither has a mask to fit or a
        // ring to sample; this function is the ladder's renderer and has
        // nothing to render for either. Falling through to rung 0 would write a
        // planar fill wearing the word `paint` in its provenance, which is the
        // same lie in the manifest the `Flux`/`Cloud` arm above exists to
        // prevent.
        Engine::Paint | Engine::Clone => {
            Ok(Rendered::Refused("decline.reason.rungUnavailable"))
        }
        // Rung 0. A fill is the answer that cannot be wrong for a region that
        // got here.
        Engine::Fill => {
            let pixels = match solid_color {
                Some(color) => {
                    let pixels=fill::render_solid_color(crop,fitted,color).map_err(|e|e.to_string())?;
                    if fill::color_is_approximated(crop,color).map_err(|e|e.to_string())? {
                        events::notice("notice.paint.colorApproximated",serde_json::json!({}),"info");
                    }
                    pixels
                },
                None => fill::render(crop, fitted),
            };
            Ok(plain(fitted.mask.clone(), pixels))
        }
    }
}

/// The catalogue key a quality decline is recorded under.
///
/// **This belongs in [`quality::Cause::reason_key`]**, and that function's own
/// documentation says so: it answers one key for both causes because the
/// catalogue held one entry, and "splitting them costs one line on the day a
/// second entry exists". The catalogue now holds both entries, and the line is
/// owed there rather than here - the core names the key for the outcome the core
/// decided. It is written here for now because rung 2's two new modules are
/// being worked on concurrently, and a run that reported an edge-energy failure
/// and a histogram failure under one key would have a reviewer looking for
/// strokes that are not there.
pub(crate) fn decline_key(cause: quality::Cause) -> &'static str {
    match cause {
        quality::Cause::EdgeEnergy => "decline.reason.edgeEnergy",
        quality::Cause::Histogram => "decline.reason.histogram",
    }
}

/// Rule 3's ownership question, asked of the **global** list: the global box
/// a detection's rectangle is in, the first that holds it whole.
///
/// A detection is in exactly one global box, and that box has one owner however
/// many segments saw it. A rectangle no global box covers is a box
/// [`merge_global`] never received, which cannot happen for a detection that
/// was in its input - the caller's fallback is "clean it here" because losing
/// a region is worse than cleaning it in the wrong segment.
fn global_of(globals: &[GlobalBox], rect: Rect) -> Option<&GlobalBox> {
    globals.iter().find(|global| {
        global.rect.x <= rect.x
            && global.rect.y <= rect.y
            && global.rect.right() >= rect.right()
            && global.rect.bottom() >= rect.bottom()
    })
}

/// What became of a deferred region once the segment `current` has run.
enum Settled {
    /// Its owner has not run yet; the owner as the evidence now stands.
    Waiting(usize),
    /// Its owner saw it and wrote it.
    Found,
    /// Its owner ran and did not see it: the segment that did writes it.
    Missed,
}

fn settle(globals: &[GlobalBox], deferred: &Deferred, current: usize) -> Settled {
    let Some(global) = global_of(globals, deferred.rect) else { return Settled::Missed };
    if global.owner > current {
        Settled::Waiting(global.owner)
    } else if global.owner != deferred.seen.index && global.segments.contains(&global.owner) {
        Settled::Found
    } else {
        Settled::Missed
    }
}

/// One deferred region per region: where segments before its owner each saw
/// it (their views merge as rule 3 merges any boxes), the latest segment's
/// view, in the order they were deferred.
fn one_per_region(deferred: Vec<Deferred>, cuts: &[Split], segments: &[Segment]) -> Vec<Deferred> {
    if deferred.len() < 2 {
        return deferred;
    }
    let evidence: Vec<DetectedInSegment> = deferred.iter().map(|deferred| deferred.found).collect();
    let globals = merge_global(&evidence, cuts, segments);
    let mut chosen: Vec<(Option<usize>, Deferred)> = Vec::with_capacity(deferred.len());
    for deferred in deferred {
        let global = globals.iter().position(|global| {
            global.rect.x <= deferred.rect.x
                && global.rect.y <= deferred.rect.y
                && global.rect.right() >= deferred.rect.right()
                && global.rect.bottom() >= deferred.rect.bottom()
        });
        match chosen.iter_mut().find(|(at, _)| global.is_some() && *at == global) {
            Some(slot) if deferred.seen.index > slot.1.seen.index => slot.1 = deferred,
            Some(_) => {}
            None => chosen.push((global, deferred)),
        }
    }
    chosen.into_iter().map(|(_, deferred)| deferred).collect()
}

/// Whether at least half of `island`'s pixels are in `masks`.
fn mostly_in<'m>(island: &Mask, masks: impl Iterator<Item = &'m Mask>) -> bool {
    let near: Vec<&Mask> = masks
        .filter(|mask| {
            let (a, b) = (&mask.bounds, &island.bounds);
            a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
        })
        .collect();
    if near.is_empty() {
        return false;
    }
    let (mut inside, mut total) = (0usize, 0usize);
    for y in island.bounds.y..island.bounds.bottom() {
        for x in island.bounds.x..island.bounds.right() {
            if island.contains(x, y) {
                total += 1;
                inside += usize::from(near.iter().any(|mask| mask.contains(x, y)));
            }
        }
    }
    inside * 2 >= total
}

/// Whether more than half of `rect` lies in `other`.
fn mostly_inside(rect: &Rect, other: &Rect) -> bool {
    let w = (other.right().min(rect.right()) - other.x.max(rect.x)).max(0);
    let h = (other.bottom().min(rect.bottom()) - other.y.max(rect.y)).max(0);
    w * h * 2 > rect.w as i64 * rect.h as i64
}

/// List a page's held candidates, each reconciled against every region the
/// page's segments found and every candidate already listed.
///
/// A candidate island (one component) at least half of whose pixels a
/// region's lettering holds is that region's, whichever segment found the
/// region, and is not listed; an island already listed from another
/// segment's view is not listed twice. What is left of a candidate is listed
/// with its reason, unless nothing is, or only specks the removed lettering
/// had beside it. A candidate with no
/// lettering (a weak text box) is dropped when most of it lies in a region
/// another segment found or in a candidate already listed.
///
/// Views are taken in a fixed order: first those whose top row is in the
/// segment that saw them (the view ownership would pick), then the rest,
/// latest segment first, each in strip order. So a candidate its owner did
/// not see is still listed, from the segment that did.
fn list_candidates(
    mut held: Vec<HeldCandidate>,
    claims: &[Claim],
    listed: &mut Vec<Listed>,
    segments: &[Segment],
    rows: &mut Vec<RegionOutcome>,
    (page_x, page_y): (i64, i64),
) {
    let home = |candidate: &HeldCandidate| {
        segments.iter().find(|segment| segment.index == candidate.segment)
            .is_some_and(|segment| i64::from(segment.start) <= candidate.rect.y && candidate.rect.y < i64::from(segment.end))
    };
    held.sort_by(|a, b| {
        home(b).cmp(&home(a))
            .then_with(|| if home(a) { std::cmp::Ordering::Equal } else { b.segment.cmp(&a.segment) })
            .then(a.rect.y.cmp(&b.rect.y))
            .then(a.rect.x.cmp(&b.rect.x))
            .then(a.segment.cmp(&b.segment))
            .then(a.rect.h.cmp(&b.rect.h))
            .then(a.rect.w.cmp(&b.rect.w))
    });
    for candidate in held {
        let (rect, islands) = if candidate.islands.is_empty() {
            let claimed = claims.iter()
                .any(|claim| claim.segment != candidate.segment && mostly_inside(&candidate.rect, &claim.rect));
            if claimed || listed.iter().any(|entry| mostly_inside(&candidate.rect, &entry.rect)) {
                continue;
            }
            (candidate.rect, Vec::new())
        } else {
            let seen = candidate.islands.len();
            let left: Vec<Mask> = candidate.islands.into_iter()
                .filter(|island| !mostly_in(island, claims.iter().map(|claim| &claim.lettering)))
                .filter(|island| !mostly_in(island, listed.iter().flat_map(|entry| entry.islands.iter())))
                .collect();
            let specks = left.iter().all(|island| island.count() < text_groups::SPECK_PIXELS as usize);
            if left.is_empty() || (left.len() < seen && specks) {
                continue;
            }
            let rect = left.iter().map(|island| island.bounds).reduce(rect_hull).expect("an island is left");
            (rect, left)
        };
        // The islands left, as one mask in page pixels: the lettering a clean
        // the user asks for writes through.
        let lettering = (!islands.is_empty()).then(|| {
            let mut mask = Mask::empty(Rect::new(rect.x - page_x, rect.y - page_y, rect.w, rect.h));
            for island in &islands {
                for y in island.bounds.y..island.bounds.bottom() {
                    for x in island.bounds.x..island.bounds.right() {
                        if island.contains(x, y) {
                            mask.set(x - page_x, y - page_y, true);
                        }
                    }
                }
            }
            (mask, candidate.scale)
        });
        rows.push(RegionOutcome::Candidate {
            bbox: Rect::new(rect.x - page_x, rect.y - page_y, rect.w, rect.h),
            reason: candidate.reason.to_owned(),
            inside_bubble: candidate.inside_bubble,
            lettering,
        });
        listed.push(Listed { rect, islands });
    }
}

/// The untouched row a held candidate is written as, with its lettering for
/// the writer to store beside it ([`Job::leave_untouched_rows`]).
pub(crate) fn held_candidate_row(
    source_idx: usize, bbox: Rect, reason: String, inside_bubble: bool, lettering: Option<(Mask, f32)>,
) -> PendingUntouched {
    PendingUntouched {
        row: RegionUntouched { source_idx, bbox, reason, inside_bubble: Some(inside_bubble), lettering: None },
        lettering,
    }
}

fn clipped_continuation(upper: &Rect, lower: &Rect, lower_crop_top: i64) -> bool {
    let shared_width = (upper.right().min(lower.right()) - upper.x.max(lower.x)).max(0);
    let shared_height = (upper.bottom().min(lower.bottom()) - upper.y.max(lower.y)).max(0);
    let x_union = upper.right().max(lower.right()) - upper.x.min(lower.x);
    shared_width * 2 >= upper.w.min(lower.w) as i64
        && (shared_height * 2 >= upper.h.min(lower.h) as i64
            || (shared_height >= strip::DETECTION_OVERLAP as i64 * 3 / 4
                && lower.y == lower_crop_top
                && shared_width * 4 >= x_union * 3))
}

fn rect_hull(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect::new(x, y, (a.right().max(b.right()) - x) as u32,
        (a.bottom().max(b.bottom()) - y) as u32)
}

/// A mask fitted in a segment crop's coordinates, in the page's.
///
/// The patch's pixels need no move: [`fill::render`] sizes its raster to the mask's bounds and the compositor places it there,
/// so the mask's rectangle is the patch's position.
/// `rect` grown by `by` on every side, unclamped: the window's own clamp
/// places it on the page.
fn padded(rect: Rect, by: u32) -> Rect {
    let by64 = i64::from(by);
    Rect::new(rect.x - by64, rect.y - by64, rect.w.saturating_add(by.saturating_mul(2)), rect.h.saturating_add(by.saturating_mul(2)))
}

fn translated(mask: Mask, dx: i64, dy: i64) -> Mask {
    Mask { bounds: Rect { x: mask.bounds.x + dx, y: mask.bounds.y + dy, ..mask.bounds }, bits: mask.bits }
}

/// The provenance snapshot a run writes.
///
/// `fill_mode` and `elapsed_ms` were left *reconstructed* - the first guessed
/// from the rung, the second defaulted to zero - because nothing wrote them.
/// The run is what knows both, and §3 calls this field "the thresholds,
/// dilations and radii **actually used**", so both go in. No manifest format
/// change was needed: `params_snapshot` is an opaque object and
/// `library::fill_mode` already prefers a recorded value over a derived one.
pub(crate) fn params_snapshot(
    engine: Engine,
    fitted: &fit::Fitted,
    elapsed: Duration,
    pad: strip::EdgePad,
    tiles: Option<u32>,
    quality: &quality::Assessment,
) -> serde_json::Value {
    let mut snapshot = serde_json::json!({
        "rung": engine.rung_key(),
        "thickness": fitted.thickness,
        "deviation": fitted.ring.deviation,
        // Rung 0 measures the paper around the mask; every rung above it
        // reconstructs what was under it. `library::fill_mode` derives the same
        // thing from the rung when the field is absent, and §3 asks for what was
        // actually used rather than for what can be inferred.
        "fill_mode": match engine {
            Engine::Fill => "match-surround",
            _ => "reconstruct",
        },
        // Which of the fit's two masks this rung wrote through
        // ("The growth is rung 0's"). Rung 0 writes the whole grown
        // mask; every rung above it writes
        // `Fitted::ink`, the lettering and its first growth step. `thickness`
        // above is the fit's own answer either way, so without this field a
        // snapshot from a model rung reads as a 64 px edit when the edit was
        // 9 - and §3 asks for the radii *actually used*. The exact set is
        // `mask_sha256`; this is the word that says which one it is.
        "write_set": match engine {
            Engine::Fill => "fitted",
            _ => "ink",
        },
        // The **region's** cost and not the accepted rung's: a region that was
        // tried at rung 0, declined, and cleaned at rung 2 cost all of it.
        "elapsed_ms": elapsed.as_millis() as u64,
        // A hand mask and an automatic mask are identical in kind, so this is
        // one word in the snapshot rather than a field.
        "source": "auto",
        // Record which edge pad was used in `params_snapshot`. `none` for an
        // interior window, which is most of them, and never `reflect` -
        // there is no such variant.
        "edge_pad": pad.as_str(),
        // Whether §6's metric actually measured this patch or had nothing to
        // measure it with: the failure to avoid is "a check that silently
        // answers accepted", and a snapshot that recorded only the accepted
        // verdicts would be it: on a bitonal source every rung passes by
        // construction, and the record should say which of the two happened.
        "quality": match quality.outcome {
            quality::Outcome::Unmeasured(_) => "unmeasured",
            _ => "measured",
        },
    });
    // Only where there is a model to have run tiles. Rungs 0 and 1 would report
    // zero, and zero reads as a model that did nothing.
    if let Some(tiles) = tiles {
        snapshot["tiles"] = tiles.into();
    }
    snapshot
}

/// `{page id}-r{n}`. Unique across the chapter, because the manifest keeps one
/// patch record per id and `library::region_of_patch` hands the id straight to
/// the seam as the region's own.
fn region_id(page_id: &str, index: usize) -> String {
    format!("{page_id}-r{index}")
}

/// The digest of the mask a patch was applied with.
///
/// Over the encoded buffer, which is what actually lands in the sidecar
/// directory, so the recorded digest and the file on disk are the same bytes.
pub(crate) fn mask_digest(mask: &Mask) -> String {
    sha256_hex(&buffers::encode_mask(mask))
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/* ------------------------------------------------------------------ */
/* One writer per job                                                  */
/* ------------------------------------------------------------------ */

pub use cleaner_core::project::lock::{JobLock, LockError};

/// How long the walk waits for another process to let go of the chapter it
/// is about to open, before it leaves that chapter's pages for a later run.
/// Longer than a command's [`cleaner_core::project::lock::PATIENCE`]: another
/// process's hand edit, a model render included, is worth waiting out, and
/// the run's cancel ends the wait whenever the user asks.
///
/// A second under test, so the busy path is walked without a minute's wait.
const RUN_PATIENCE: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(60) };

/// Take a job for writing, waiting for whichever thread has it and a few
/// seconds for another process ([`cleaner_core::project::lock::PATIENCE`]).
///
/// **What this is, exactly.** A manifest is rewritten whole, so every write
/// to one is a read-modify-write over the entire file, and two of them that
/// interleave lose
/// the earlier one entirely. Two things made that worth closing now rather than
/// later: the run holds one [`Job`] in memory for a **whole chapter** and
/// rewrites the manifest after every region, so the window it is exposed for
/// went from milliseconds to the length of a run; and the run is the first
/// writer that is not on the command thread, so "two commands cannot overlap"
/// stopped being true by construction.
///
/// **Across processes, too.** This was once a lock within one process, and a
/// headless runner beside the open GUI lost two committed cloud patches to a
/// GUI edit that saved a manifest it had read seconds before. It is now
/// [`cleaner_core::project::lock::lock`]: this process's slot, then an OS
/// advisory lock on `<job>.lock` that the kernel releases when a holder exits,
/// so there is no owner file to trust and no staleness rule. Hold it across
/// the whole read-modify-write - open, change, save, and any sidecar written
/// with the save.
///
/// **Another process can hold a chapter for a long time**: a headless run
/// holds its chapter from the first page to the last. So the wait on one is
/// bounded, and [`LockError::Busy`] comes back instead of a command that never
/// answers; its message starts with the code `job_busy`, which the interface
/// says as "another Manga Cleaner process is using this chapter".
///
/// **What a holder must not do** is wait on a thread that needs this same job,
/// which is the deadlock the cloud analysis producer once had with the walk
/// ([`crate::inference::run_analysis`] reads the manifest without it), or hold
/// a process-wide lock while it waits here, which stalls every command that
/// needs that lock behind another process's run. It is not reentrant: asking
/// twice on one thread panics rather than hangs.
///
/// A read needs none of it. Every manifest is replaced whole by a rename, so a
/// read without the lock sees one whole save or the next, and [`Job::open`]
/// takes the lock itself for the one thing it writes.
pub fn lock_job(path: &Path) -> Result<JobLock, LockError> {
    cleaner_core::project::lock::lock(path)
}

/// The job to close or to go on with. `job` itself, unless a save from it was
/// refused as stale: then the manifest the other writer saved, read again
/// under the lock the caller still holds, so what the run does next (a page,
/// or the closing `interrupted_at` in [`close`]) lands on it instead of being
/// skipped. `None` when it cannot be read any more.
fn current(path: &Path, job: Job) -> Option<Job> {
    if !job.went_stale() {
        return Some(job);
    }
    eprintln!("manga-cleaner: {} changed under the run; reading it again", path.display());
    Job::open(path).ok()
}

/* ------------------------------------------------------------------ */
/* The queue                                                           */
/* ------------------------------------------------------------------ */

/// One page of a run: which chapter it belongs to, which job holds it, and
/// where it sits in that job's `strip.order`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    pub chapter_id: String,
    pub job_path: PathBuf,
    pub page_index: usize,
    pub source_idx: usize,
}

/* ------------------------------------------------------------------ */
/* The strip, surveyed once per job                                    */
/* ------------------------------------------------------------------ */

/// A job's strip and everything one walk over its pages could establish.
///
/// One of these per job per run, built when the job is opened and handed to
/// every page as a [`PageContext`].
pub struct JobStrip {
    pub strip: Strip,
    pub survey: Survey,
    pub sources: Vec<Option<PathBuf>>,
}

/// The manifest's `sources` and `strip.order`, as rule 1's coordinate system.
///
/// A position in `order` that names no source keeps its place as a zero-sized
/// page rather than being dropped, so a placement index is a position in
/// `strip.order` - which is what [`PlanEntry::page_index`] is, and what every
/// event on the seam carries.
pub fn strip_of(project: &Project) -> Strip {
    cleaner_core::project::orientation::strip(project, true)
}

/// Whether this job's pages have to be read before they are cleaned.
///
/// The survey costs one decode of every page, so it is taken where it buys
/// something and not otherwise:
///
/// - a **longstrip** project, because that is where the join check applies
///   and where rule 1 lets a read cross a join at all;
/// - a project any of whose **sources is itself a long strip**, because rule 2
///   has to split it and rule 3 has to segment it, and an unsurveyed profile
///   would fall back to a hard cut every 2000 px on evidence nothing gathered.
///
/// A chapter of ordinary pages is neither, and pays nothing: its pages are its
/// segments, which is rule 2's own sentence for the case its trigger does not
/// fire.
fn survey_is_worth_it(project: &Project) -> bool {
    project.strip.mode == StripMode::Longstrip
        || project
            .sources
            .iter()
            .any(|s| {let (w,h)=s.orientation.size(s.w,s.h);h>0 && (w as f32/h as f32)<LONGSTRIP_ASPECT})
}

/// Survey a job, report the joins that failed, and record the split plan.
///
/// **This report satisfies one half of what's needed.** `check_join` and
/// `Joins::anomalies` had no caller outside the tests, so "report them" was
/// unmet for four of four variants - including the one the catalogue can say.
/// It is met here for that one. The other three still have no sentence to be
/// reported in and `JoinAnomaly::reason_key` still answers `None` for them,
/// which is the remaining half - a vocabulary decision the core may not take.
/// What is *not* silent either way is the behaviour: an anomalous join is not
/// verified, so no read crosses it, notice or no notice.
///
/// The plan is written to `strip.splits` only for a `Longstrip` project,
/// because [`cleaner_core::project::Strip::splits`] is defined as "always empty
/// for `Single`, where the page boundaries are the splits" - and on a `Single`
/// project every split rule 2 finds *is* a page boundary, so the field would
/// say nothing the order does not.
fn survey_job(job: &mut Job, emit: &dyn Fn(Event)) -> JobStrip {
    let strip = strip_of(&job.project);
    let longstrip = job.project.strip.mode == StripMode::Longstrip;

    let sources: Vec<Option<PathBuf>> = (0..strip.pages().len())
            .map(|page| {
                library::Library::resolve_page(&job.project, page)
                    .and_then(|source| job.source_path(source))
            })
            .collect();
    let mut survey = if survey_is_worth_it(&job.project) {
        strip::survey(&strip, |placement| {
            let path = sources.get(placement)?.as_ref()?;
            let bytes = std::fs::read(path).ok()?;
            decode(&bytes).ok().map(|p| cleaner_core::image::orientation::from_bytes(&bytes).raster(&p))
        })
    } else {
        // The joins stay unchecked - which rule 1 reads as a page edge, and no
        // read crosses one - and the profile stays empty, which rule 2 only
        // ever consults for a strip its trigger fires on.
        strip::survey(&strip, |_| None)
    };

    if longstrip {
        for segment in &mut survey.segments {
            if let Some(join) = (0..strip.joins()).find(|j| strip.join_row(*j) == Some(segment.end)) {
                if survey.joins.is_verified(join) {
                    segment.detect_end = segment.end.saturating_add(strip::DETECTION_OVERLAP)
                        .min(strip.height());
                }
            }
        }
        for (join, anomaly) in survey.anomalies() {
            if let Some(key) = anomaly.reason_key() {
                emit(events::notice_event(
                    key,
                    serde_json::json!({ "first": join + 1, "second": join + 2 }),
                    "warn",
                ));
            }
        }
        if job.project.strip.splits != survey.splits {
            job.project.strip.splits = survey.splits.clone();
            let _ = job.flush();
        }
    }

    JobStrip { strip, survey, sources }
}

/// Whether a page is work a run has left to do.
///
/// **Examined or detected, and nothing else.** Having a patch on it is not the test, and
/// that is the whole point of the field: a page the run got three regions into
/// before it was cancelled has patches and is not finished, and a page with no
/// text on it has none and is. Reading "has patches" as "done" would strand the
/// first case - cancel keeps the completed regions and the resume must still
/// redo the page - and reading "has no patches" as "to do" would requeue the
/// second one on every run for ever.
///
/// **Which work depends on the mode**, and stored detections are the reason.
/// Detect looks at every page nothing has examined or detected. Clean has work
/// only where detections wait. Auto has both: a page with detections waiting
/// is work (Detect marks a page examined only when it found nothing on it),
/// and so is any other page nothing has examined or detected.
///
/// **A page Detect finished is done for Detect** (`Project::detected`), so a
/// Detect that stopped part way - cancelled, or ended by a cloud failure -
/// resumes where it stopped instead of starting the chapter over. `redetect`
/// lifts that for one page asked for by name: detecting it again replaces
/// what it stored, the way to see a page's detection with other settings.
fn runnable(project: &Project, source_idx: usize, mode: RunMode, redetect: bool) -> bool {
    let unlooked = !project.examined.contains(&source_idx)
        && (redetect || !project.detected.contains(&source_idx));
    match mode {
        RunMode::Detect => unlooked,
        RunMode::Clean => has_detections(project, source_idx),
        RunMode::Auto => unlooked || has_detections(project, source_idx),
    }
}

/// Whether a page has stored detections waiting to be cleaned.
fn has_detections(project: &Project, source_idx: usize) -> bool {
    project.detections.iter().any(|row| row.source_idx == source_idx)
}

/// Drop the automatic outcomes a previous pass left on this page.
///
/// `Job::complete_region` replaces a patch record with the same id, so re-running
/// a page cannot double its patches - but `Job::leave_untouched` appends, and a
/// page redone after a cancel would collect a second copy of every region the
/// gate held back. So a page starts from a clean slate for the work a run owns:
/// this source's untouched rows, and the patches whose ids this run issued.
///
/// Nothing else is touched. A hand-drawn region carries an id a run never
/// mints, and a hand mask and an automatic mask are identical in kind -
/// identical in kind, not in provenance, and a run has no business deleting
/// an edit a person made.
///
/// **The counters come back down with the rows.** `counters` is the basis
/// of honest statistics, and a counter that only ever goes up is not one:
/// dropping a page's `regions_untouched` rows while leaving `gate_dropped`
/// where it was meant that every cancel-and-resume added the same regions
/// again, for ever, and a page that failed added to `errored` on every
/// attempt. So each row's contribution is subtracted as the row goes, by the
/// same test that put it there - the counters state what the manifest
/// currently holds, and a resumed run ends on the same numbers an
/// uninterrupted one does.
fn reset_page(job: &mut Job, source_idx: usize, page_id: &str) {
    let prefix = format!("{page_id}-r");
    let before = (job.project.patches.len(), job.project.regions_untouched.len());
    job.project
        .patches
        .retain(|record| record.source_idx != source_idx || !record.id.starts_with(&prefix));

    let (mut gate_dropped, mut declined) = (0u32, 0u32);
    job.project.regions_untouched.retain(|record| {
        if record.source_idx != source_idx {
            return true;
        }
        // The same classification `execute` counted it under, so the two
        // cannot drift into counting one thing up and another down. A held
        // candidate (it records its balloon) was counted as neither.
        if record.inside_bubble.is_some() {
        } else if is_gate_skip(&record.reason) {
            gate_dropped += 1;
        } else {
            declined += 1;
        }
        false
    });
    let counters = &mut job.project.counters;
    counters.gate_dropped = counters.gate_dropped.saturating_sub(gate_dropped);
    counters.declined = counters.declined.saturating_sub(declined);

    let unerrored = job.clear_errored(source_idx);
    if unerrored || before != (job.project.patches.len(), job.project.regions_untouched.len()) {
        let _ = job.flush();
    }
}

/// Whether an untouched region's reason is the gate holding it back, rather
/// than a route nothing would run. One test, used where a region is counted and
/// again where it is uncounted.
pub(crate) fn is_gate_skip(reason: &str) -> bool {
    reason.contains("gateSkipped")
        || matches!(reason, "review.reason.languageSkipped" | "review.reason.outsideLanguageUnverified")
}

/// The queue a scope asks for, in order.
///
/// `'project'` walks every chapter of the project the chapter belongs to;
/// `'chapter'` walks one; `'page'` walks the one page. A chapter with no job on
/// disk contributes nothing rather than failing the call - it is a chapter
/// nobody has put pages in, which is a listing state, not an error.
///
/// This is Auto's queue, which is also the widest: every page Detect or Clean
/// would take is in it, and so a cloud consent proposed from it covers the
/// pages any mode's run will ask the grant for. [`plan_for`] is the queue for a
/// mode.
pub fn plan(
    library: &Library,
    scope: &str,
    chapter_id: &str,
    page_index: Option<u32>,
) -> Result<Vec<PlanEntry>, LibraryError> {
    plan_for(library, scope, chapter_id, page_index, RunMode::Auto)
}

/// The queue a scope asks for, in order, for a run of this mode. See
/// [`runnable`] for what each mode counts as work.
pub fn plan_for(
    library: &Library,
    scope: &str,
    chapter_id: &str,
    page_index: Option<u32>,
    mode: RunMode,
) -> Result<Vec<PlanEntry>, LibraryError> {
    plan_with_repeat(library, scope, chapter_id, page_index, mode, false)
}

pub(crate) fn plan_cloud_detection(
    library: &Library, scope: &str, chapter_id: &str, page_index: Option<u32>,
) -> Result<Vec<PlanEntry>, LibraryError> {
    plan_with_repeat(library, scope, chapter_id, page_index, RunMode::Detect, true)
}

fn plan_with_repeat(
    library: &Library, scope: &str, chapter_id: &str, page_index: Option<u32>,
    mode: RunMode, repeat: bool,
) -> Result<Vec<PlanEntry>, LibraryError> {
    let index = library.index()?;
    let Some(project) =
        index.projects.iter().find(|p| p.chapters.iter().any(|c| c.id == chapter_id))
    else {
        return Err(LibraryError::Unknown { id: chapter_id.to_owned() });
    };

    let chapters: Vec<&str> = if scope == "project" {
        project.chapters.iter().map(|c| c.id.as_str()).collect()
    } else {
        vec![chapter_id]
    };

    let mut entries = Vec::new();
    for id in chapters {
        let job_path = library.job_path(&project.id, id);
        let Ok(job) = Job::open(&job_path) else { continue };
        for page in 0..job.project.strip.order.len() {
            if scope == "page" && Some(page as u32) != page_index {
                continue;
            }
            let Some(source_idx) = Library::resolve_page(&job.project, page) else { continue };
            if repeat || runnable(&job.project, source_idx, mode, scope == "page") {
                entries.push(PlanEntry {
                    chapter_id: id.to_owned(),
                    job_path: job_path.clone(),
                    page_index: page,
                    source_idx,
                });
            }
        }
    }
    Ok(entries)
}

/* ------------------------------------------------------------------ */
/* The scheduler                                                       */
/* ------------------------------------------------------------------ */

/// What [`execute`] answers with. The event stream is the product; this is what
/// the caller needs to update the manifest's interrupted marker and to notice.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub reason: &'static str,
    pub pages_queued: u32,
    pub pages_cleaned: u32,
    pub regions_cleaned: u32,
    pub next_page_index: Option<u32>,
    /// Pages whose clean failed outright. Counted into the manifest's
    /// `counters.errored` and reported nowhere else - see the note on the
    /// missing catalogue key in [`run_clean`].
    pub errored: u32,
    /// Regions a Detect run stored. Zero for every other mode: a clean never
    /// detects, and Auto cleans what it detects in the same pass.
    pub regions_detected: u32,
}

/// Run a queue, emitting as it goes.
///
/// The whole ordering contract lives in this function, and each rule is one
/// line of it:
///
/// - `page-started` goes out before any of that page's regions, and
///   `page-done` after all of them, so **every `region-done` falls strictly
///   between them**.
/// - `run-finished` is emitted **exactly once**, on the single exit, whatever
///   happened. There is no early return.
/// - `next_page_index` is `Some` **only** when the run was cancelled, and it is
///   the page the run stopped *at* rather than the last one it finished - that
///   page is unfinished, and a resume must redo it.
/// - a cancel that lands mid-page emits **no** `page-done` for that page, and
///   does not mark it examined, so the page is still queued afterwards. The
///   regions already recorded stay recorded: cancel keeps completed regions.
///
/// "Whatever happened" includes a **panic**, and that is what the
/// [`catch_unwind`](std::panic::catch_unwind) here is for. A panic in the
/// pipeline - a model that produced a shape nothing expected, an arithmetic
/// overflow in a fit - used to unwind straight past this function's single
/// exit, and the consequences were all of the seam's: no `run-finished` ever,
/// so the page kept its `●` and the run never ended; the active slot never
/// cleared, so every later `runClean` answered `alreadyRunning` for the rest of
/// the session; and `cancelRun` waiting the full [`CANCEL_WAIT`] before naming a
/// run that had already stopped. A panicked run reports itself as **cancelled**,
/// which is the true half of what happened and the only other word `reason`
/// can give: the run stopped early, every completed region is kept, and the
/// page it stopped inside is the resume point. What it cannot say is that it
/// *failed* - the catalogue has no key for it.
pub fn execute(
    run_id: &str,
    chapter_id: &str,
    entries: &[PlanEntry],
    cleaner: &mut dyn Cleaner,
    ceiling: Engine,
    cancel: &AtomicBool,
    emit: &dyn Fn(Event),
) -> Summary {
    execute_as(RunMode::Auto, run_id, chapter_id, entries, cleaner, ceiling, cancel, emit)
}

/// [`execute`] for a run of a given mode. Every ordering rule above holds for
/// each of them; what the mode changes is what a page's work is (see
/// [`walk`]).
#[allow(clippy::too_many_arguments)]
pub fn execute_as(
    mode: RunMode,
    run_id: &str,
    chapter_id: &str,
    entries: &[PlanEntry],
    cleaner: &mut dyn Cleaner,
    ceiling: Engine,
    cancel: &AtomicBool,
    emit: &dyn Fn(Event),
) -> Summary {
    let mut progress = Progress {
        summary: Summary {
            reason: "completed",
            pages_queued: entries.len() as u32,
            pages_cleaned: 0,
            regions_cleaned: 0,
            next_page_index: None,
            errored: 0,
            regions_detected: 0,
        },
        in_flight: None,
    };

    let walked = std::panic::catch_unwind(AssertUnwindSafe(|| {
        walk(mode, run_id, entries, cleaner, ceiling, cancel, emit, &mut progress);
    }));

    let mut summary = progress.summary;
    if walked.is_err() {
        summary.reason = "cancelled";
        // The walk's own `close` never ran, so the job on disk does not know it
        // was interrupted and Home would offer no resume for a run that stopped
        // half way through a chapter. The page in flight is where one picks up:
        // it was not marked examined, so it is queued either way, and this is
        // what puts the offer in front of the user.
        if let Some((job_path, page_index)) = progress.in_flight {
            summary.next_page_index = Some(page_index);
            if let Ok(_lock) = lock_job(&job_path) {
                if let Ok(mut job) = Job::open(&job_path) {
                    let _ = job.mark_interrupted(Some(page_index));
                }
            }
        }
    }

    emit(Event::RunFinished {
        run_id: run_id.to_owned(),
        chapter_id: chapter_id.to_owned(),
        reason: summary.reason,
        pages_queued: summary.pages_queued,
        pages_cleaned: summary.pages_cleaned,
        regions_cleaned: summary.regions_cleaned,
        next_page_index: summary.next_page_index,
    });
    summary
}

/// What the walk has done, and where it is.
///
/// One struct rather than seven arguments, and the second field is the reason
/// it exists at all: a run that does not come back has to leave behind the page
/// it was inside, or nothing can say where to resume.
struct Progress {
    summary: Summary,
    /// The job and the `strip.order` position the walk is inside, or `None`
    /// between pages.
    in_flight: Option<(PathBuf, u32)>,
}

/// What one page of a walk does, from the run's mode and what the page holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PageWork {
    /// Detect and clean in one pass: Auto on a page with nothing waiting.
    Both,
    /// Detect and store.
    Detect,
    /// Clean what is stored.
    Clean,
}

/// Each result of a page's work, with the detection it replaces where it
/// replaces one, and the pressure steps the page took.
type PageWorked = (Vec<(Option<String>, RegionOutcome, Option<Rect>)>, Vec<&'static str>, Vec<EvidenceBlob>);

/// The queue itself. Every ordering rule [`execute`] documents is here; the one
/// rule that is not - exactly one `run-finished` - is there, because it has to
/// hold for the exits this function does not take.
///
/// **What a page's work is comes from the mode and from the page**, decided
/// when the walk reaches it rather than when the queue was built, because the
/// manifest is the truth and a detection can be deleted in between:
///
/// - One pass ([`PageWork::Both`]) is the walk as it always was.
/// - Detect resets the page's detections and untouched rows, keeps its
///   patches, stores what the pipeline found, and does **not** mark the page
///   examined: it is not finished until it is cleaned.
/// - Clean loads the page's detections and replaces each one with its patch or
///   untouched row, one manifest write per region, and marks the page examined
///   only once none is left - a cancel mid-page leaves the rest waiting, which
///   is the resume point. A page that had none by the time the walk reached it
///   is announced and finished with nothing done, so the interface does not
///   keep it marked as queued.
#[allow(clippy::too_many_arguments)]
fn walk(
    mode: RunMode,
    run_id: &str,
    entries: &[PlanEntry],
    cleaner: &mut dyn Cleaner,
    ceiling: Engine,
    cancel: &AtomicBool,
    emit: &dyn Fn(Event),
    progress: &mut Progress,
) {
    let summary = &mut progress.summary;
    let mut open: Option<(PathBuf, JobLock, Job, JobStrip)> = None;
    /* Which model faults this run has already told the user about, by the pair
     * of keys the notice is built from. A dead device fails *every* page of the
     * chapter, and three hundred copies of one sentence on the notice stack is
     * not three hundred pieces of information - it is one, said in a way that
     * buries everything else the run had to say. The stderr line below is not
     * deduplicated for the opposite reason: it names the page, so each one is a
     * different fact. */
    let mut announced: HashSet<(&'static str, &'static str)> = HashSet::new();
    let mut busy: HashSet<PathBuf> = HashSet::new();

    for entry in entries {
        if cancel.load(Ordering::SeqCst) {
            summary.next_page_index = Some(entry.page_index as u32);
            break;
        }

        // One job open at a time. A `'project'` run walks chapters in order, so
        // this reopens once per chapter rather than once per page. The lock is
        // taken with the job and held for as long as it is open, because that
        // whole span is one read-modify-write over the manifest.
        //
        // The strip is surveyed here, once per job, for the same reason: rules
        // 1 to 3 are properties of the chapter and not of the page, and a
        // survey per page would decode the chapter once per page.
        //
        // A job whose save was refused as stale is not written again: another
        // writer saved the manifest after this run read it, which only one
        // that skips the lock can do. It is read again under the lock this
        // walk still holds, so what the other writer committed stays, the page
        // that failed is left for a later run, and the chapter is still closed.
        match open.take() {
            Some((path, lock, job, geometry)) if path == entry.job_path => {
                if job.went_stale() {
                    let Some(mut fresh) = current(&path, job) else {
                        summary.errored += 1;
                        continue;
                    };
                    let geometry = survey_job(&mut fresh, emit);
                    open = Some((path, lock, fresh, geometry));
                } else {
                    open = Some((path, lock, job, geometry));
                }
            }
            previous => {
                if let Some((path, _lock, job, _)) = previous {
                    close(&path, job, None);
                }
                // A chapter another process is using is waited on once, not
                // once per page: its pages are left for a later run.
                if busy.contains(&entry.job_path) {
                    summary.errored += 1;
                    continue;
                }
                let lock = match cleaner_core::project::lock::lock_until(&entry.job_path, RUN_PATIENCE, &|| {
                    cancel.load(Ordering::SeqCst)
                }) {
                    Ok(lock) => lock,
                    Err(LockError::Stopped { .. }) => {
                        summary.next_page_index = Some(entry.page_index as u32);
                        break;
                    }
                    Err(LockError::Busy { .. }) => {
                        emit(events::notice_event("notice.job.busy", serde_json::json!({}), "warn"));
                        busy.insert(entry.job_path.clone());
                        summary.errored += 1;
                        continue;
                    }
                };
                match Job::open(&entry.job_path) {
                    Ok(mut job) => {
                        let geometry = survey_job(&mut job, emit);
                        open = Some((entry.job_path.clone(), lock, job, geometry));
                    }
                    Err(_) => {
                        summary.errored += 1;
                        continue;
                    }
                }
            }
        }
        let Some((_, _, job, geometry)) = open.as_mut() else { continue };

        let page_id = library::page_id(&entry.chapter_id, entry.page_index);
        let repeat_cloud = mode == RunMode::Detect && cleaner.cloud_detection();
        let area_only = mode == RunMode::Detect && cleaner.area_only();
        let waiting = has_detections(&job.project, entry.source_idx);
        let work = match mode {
            RunMode::Detect => PageWork::Detect,
            RunMode::Clean => PageWork::Clean,
            RunMode::Auto if waiting => PageWork::Clean,
            RunMode::Auto => PageWork::Both,
        };
        match work {
            PageWork::Both => reset_page(job, entry.source_idx, &page_id),
            PageWork::Detect => {},
            // Nothing a clean owns is reset: the detections are its input, and
            // every patch and untouched row on the page is someone's result.
            PageWork::Clean => {
                if job.clear_errored(entry.source_idx) {
                    let _ = job.flush();
                }
            }
        }
        // What one pass must not clean again: the patches the page still holds
        // after its reset, which are cleaned detections (`-d` ids) and hand
        // edits, a deleted one included, since a deleted mask is text the user
        // chose to keep. Detect skips the same regions
        // ([`Job::detection_is_cleaned_already`]); one pass reaches them when a
        // page's detections were cleaned or deleted and it was never examined.
        // Taken before the pass, so its own new patches never count.
        let cleaned_before: Vec<Rect> = if work == PageWork::Both {
            job.project.patches.iter()
                .filter(|record| record.source_idx == entry.source_idx)
                .map(|record| cleaner_core::project::orientation::display_bbox(&job.project,record))
                .collect()
        } else {
            Vec::new()
        };
        progress.in_flight = Some((entry.job_path.clone(), entry.page_index as u32));
        emit(Event::PageStarted {
            run_id: run_id.to_owned(),
            chapter_id: entry.chapter_id.clone(),
            page_id: page_id.clone(),
            page_index: entry.page_index as u32,
        });

        if work == PageWork::Clean && !waiting {
            emit_page_done(emit, run_id, entry, &page_id, job);
            progress.in_flight = None;
            continue;
        }

        let bytes = job
            .source_path(entry.source_idx)
            .ok_or_else(|| "no such source".to_owned())
            .and_then(|path| std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display())));
        let context = PageContext {
            strip: &geometry.strip,
            joins: &geometry.survey.joins,
            segments: &geometry.survey.segments,
            placement: entry.page_index,
            sources: &geometry.sources,
        };
        // Every shape of work answers with the same list: each result, and the
        // detection it replaces where it replaces one. Bound before the match
        // rather than matched on directly, because the failure arm asks the
        // cleaner a second question and the closure would still be holding it.
        let worked: Result<PageWorked, String> =
            bytes.and_then(|bytes| match work {
                PageWork::Both | PageWork::Detect => {
                    let outcome = if work == PageWork::Both {
                        cleaner.clean_page(&page_id, &bytes, ceiling, &context)
                    } else {
                        cleaner.detect_current_page(&page_id, &bytes, ceiling, &context, job)
                    }?;
                    Ok((outcome.regions.into_iter().map(|region| {
                        let bbox = match &region {
                            RegionOutcome::Cleaned(patch, _) => outcome.detected_boxes.get(&patch.id).copied(),
                            _ => None,
                        };
                        (None, region, bbox)
                    }).collect(), outcome.pressure, outcome.evidence))
                }
                PageWork::Clean => {
                    let loaded = page_detections(job, entry.source_idx)?;
                    let outcome = cleaner.clean_detections(&page_id, &bytes, ceiling, &context, &loaded)?;
                    Ok((outcome.regions.into_iter().map(|(id, region)| (Some(id), region, None)).collect(),
                        outcome.pressure, Vec::new()))
                }
            });
        let (regions, pressure, evidence) = match worked {
            Ok(outcome) => outcome,
            Err(detail) => {
                // **The error is not thrown away any more.** It used to be
                // discarded here - never logged, never emitted, never stored -
                // and the consequence was at the far end of the run: every page
                // failing left `regions_cleaned` at zero, and [`report`] read
                // that as an empty chapter and told the user there was no text
                // to clean. A dead GPU reported itself as a quiet page.
                //
                // Two things go out instead. The raw text goes to stderr, named
                // by page, because it is ONNX Runtime's own words and no
                // catalogue key can carry them across the seam. And a fault the
                // pipeline could attribute becomes a notice built from the two
                // keys it *can* carry - which model, and which provider - so
                // the user reads a sentence about their hardware rather than a
                // provider string they cannot act on.
                eprintln!("manga-cleaner: {page_id} could not be cleaned: {detail}");
                if let Some(fault) = cleaner.last_fault() {
                    if announced.insert((fault.model_key, fault.accel_key)) {
                        emit(events::notice_event(
                            "notice.run.engineFault",
                            serde_json::json!({
                                "modelKey": fault.model_key,
                                "accelKey": fault.accel_key,
                            }),
                            "warn",
                        ));
                    }
                }
                fail_page(emit, run_id, entry, &page_id, job, summary);
                progress.in_flight = None;
                // The pages after it are left unexamined, so a later run
                // picks them up; this one has no authority to send them.
                if let Some(code) = cleaner.stop_reason() {
                    emit(events::notice_event(
                        "cloud.analysis.run.stopped",
                        serde_json::json!({ "page": entry.page_index + 1, "code": code }),
                        "warn",
                    ));
                    break;
                }
                continue;
            }
        };

        // The pressure ladder is reported before the regions it changed the
        // outcome of are: a run that quietly stopped
        // using the inpainter looks like a run whose pages got easier.
        for step in &pressure {
            emit(events::notice_event(step, serde_json::json!({}), "warn"));
        }

        let mut pending_detections = Vec::new();
        let mut pending_untouched: Vec<PendingUntouched> = Vec::new();
        let mut cancelled_here = false;
        let mut failed_here = false;
        for (replaces, region, detected_bbox) in regions {
            if cancel.load(Ordering::SeqCst) {
                cancelled_here = true;
                break;
            }
            match region {
                RegionOutcome::Cleaned(patch, review_state) => {
                    let detected_bbox = detected_bbox.unwrap_or(patch.mask.bounds);
                    if replaces.is_none() && cleaned_before.iter()
                        .any(|kept| cleaner_core::project::half_inside(detected_bbox, *kept)) {
                        continue;
                    }
                    let mut patch = *patch;
                    patch.provenance.mask_sha256 = mask_digest(&patch.mask);
                    let record = PatchRecord::of(entry.source_idx, &patch, review_state.clone());
                    // The group's evidence file is on disk before the row
                    // that names it, as Detect writes it.
                    let named = patch.provenance.params_snapshot["group"]["evidence"].as_str();
                    let written = named.and_then(|reference| evidence.iter().find(|blob| blob.reference() == reference))
                        .map_or(Ok(()), |blob| job.write_evidence(blob));
                    // A cleaned detection leaves in the same write that names
                    // its patch. A detection that is no longer there - nothing
                    // in this build removes one under the job's lock, but the
                    // manifest is the truth - still gets its patch.
                    let written = written.and_then(|()| match replaces.as_deref() {
                        Some(id) => job.complete_detection(id, &patch, review_state.clone())
                            .and_then(|replaced| if replaced { Ok(()) } else {
                                job.complete_display_region(entry.source_idx, &patch, review_state)
                            }),
                        None => job.complete_display_region(entry.source_idx, &patch, review_state),
                    });
                    if written.is_err() {
                        // The region's pixels or the manifest could not be
                        // written - a full disk is the shape this takes, and on
                        // one every region of every page fails. Swallowing it
                        // lost the region for good: the page was marked
                        // examined regardless, so no later run would ever
                        // re-queue it, and a chapter every region of which had
                        // failed reported `noTextDetected` - the field saying the
                        // opposite of what happened. It is the same failure as
                        // a page that could not be read, so it takes the same
                        // path: counted, **not** examined, and
                        // left in the queue with what it managed to write.
                        failed_here = true;
                        break;
                    }
                    summary.regions_cleaned += 1;
                    let page = page_or_stub(entry, &page_id, job);
                    emit(Event::RegionDone {
                        run_id: run_id.to_owned(),
                        chapter_id: entry.chapter_id.clone(),
                        page_id: page_id.clone(),
                        page_index: entry.page_index as u32,
                        region: Box::new(library::region_of_patch(&record, &page)),
                    });
                }
                // Held, never cleaned: written with the page's other
                // untouched rows below, or with its detections.
                RegionOutcome::Candidate { bbox, reason, inside_bubble, lettering } => {
                    pending_untouched.push(held_candidate_row(entry.source_idx, bbox, reason, inside_bubble, lettering));
                }
                RegionOutcome::Untouched { bbox, reason } => {
                    if work == PageWork::Detect {
                        pending_untouched.push(RegionUntouched {
                            source_idx: entry.source_idx, bbox, reason, inside_bubble: None, lettering: None,
                        }.into());
                        continue;
                    }
                    if is_gate_skip(&reason) {
                        job.project.counters.gate_dropped += 1;
                    } else {
                        job.project.counters.declined += 1;
                    }
                    // A stored detection declines in its own write; a page's
                    // other untouched rows are written together below, not
                    // one manifest flush each.
                    let Some(id) = replaces.as_deref() else {
                        pending_untouched.push(RegionUntouched {
                            source_idx: entry.source_idx, bbox, reason, inside_bubble: None, lettering: None,
                        }.into());
                        continue;
                    };
                    if job.decline_detection(id, &reason).is_err() {
                        failed_here = true;
                        break;
                    }
                }
                // A one-pass decline: written with the page's other untouched
                // rows below, keeping its lettering as a held candidate does.
                RegionOutcome::Declined { bbox, reason, lettering } => {
                    job.project.counters.declined += 1;
                    if let Some(id) = replaces.as_deref() {
                        if job.decline_detection(id, &reason).is_err() {
                            failed_here = true;
                            break;
                        }
                        continue;
                    }
                    pending_untouched.push(PendingUntouched {
                        row: RegionUntouched {
                            source_idx: entry.source_idx, bbox, reason, inside_bubble: None, lettering: None,
                        },
                        lettering,
                    });
                }
                // Stored rather than cleaned. No `region-done`: that event is a
                // cleaned region's, and the page's `page-done` carries every
                // detection the page now holds.
                RegionOutcome::Detected(found) => {
                    let found = *found;
                    if !repeat_cloud && job.detection_is_cleaned_already(entry.source_idx, found.record.bbox) {
                        continue;
                    }
                    pending_detections.push(found);
                }
            }
        }

        if work != PageWork::Detect {
            // Best effort, as each row always was: the rows already cleaned
            // or declined stand either way.
            let _ = job.leave_untouched_rows(std::mem::take(&mut pending_untouched));
        }
        if cancelled_here {
            summary.next_page_index = Some(entry.page_index as u32);
            break;
        }
        if failed_here {
            fail_page(emit, run_id, entry, &page_id, job, summary);
            progress.in_flight = None;
            continue;
        }
        if area_only {
            // Text the page holds already is not stored again, by pixel: see
            // [`crate::region::area_detection_less_held`]. A mask that cannot
            // be read fails the page, as it fails a clean.
            let held = page_detections(job, entry.source_idx).ok()
                .zip(crate::region::page_size(job, entry.source_idx));
            let Some((held, size)) = held else {
                fail_page(emit, run_id, entry, &page_id, job, summary);
                progress.in_flight = None;
                continue;
            };
            pending_detections = pending_detections.into_iter()
                .filter_map(|found| crate::region::area_detection_less_held(found, &held, size))
                .collect();
        }
        if work == PageWork::Detect {
            // What the padding ran together is stored as one detection, as a
            // later change of padding would leave it
            // ([`crate::region::merged_by_padding`]).
            if let Some(size) = crate::region::page_size(job, entry.source_idx) {
                pending_detections = crate::region::merged_by_padding(pending_detections, size);
            }
            let count = pending_detections.len();
            // An area's results join what the page holds, less what it holds
            // already; a page's replace them, or join them after a cloud pass.
            let stored = if area_only {
                job.add_area_detection_results(entry.source_idx, &page_id, pending_detections, pending_untouched)
            } else if repeat_cloud {
                job.append_page_detection_results(entry.source_idx, &page_id, pending_detections, pending_untouched)
                    .map(|()| count)
            } else {
                job.replace_page_detection_results(entry.source_idx, &page_id, pending_detections, pending_untouched)
                    .map(|()| count)
            };
            let Ok(stored) = stored else {
                fail_page(emit, run_id, entry, &page_id, job, summary);
                progress.in_flight = None;
                continue;
            };
            summary.regions_detected += stored as u32;
        }

        // Detect has looked and cleaned nothing, so a page it found text on is
        // still work; one it found none on is finished, as a one pass would
        // leave it. A clean that left a detection behind has not finished the
        // page either.
        // An area is not the page: a Detect held to one finishes nothing.
        let finished = match work {
            PageWork::Both => true,
            PageWork::Detect if area_only => false,
            PageWork::Detect | PageWork::Clean => !has_detections(&job.project, entry.source_idx),
        };
        if finished {
            let _ = job.mark_examined(entry.source_idx);
        }
        emit_page_done(emit, run_id, entry, &page_id, job);
        summary.pages_cleaned += 1;
        progress.in_flight = None;
    }

    if summary.next_page_index.is_some() {
        summary.reason = "cancelled";
    }
    if let Some((path, _lock, job, _)) = open.take() {
        close(&path, job, summary.next_page_index);
    }
}

/// A page's stored detections with their masks, in reading order. A mask file
/// that cannot be read fails the page, which leaves the detection in place for
/// the user to delete or detect again: cleaning something other than what was
/// stored would be a guess.
fn page_detections(job: &Job, source_idx: usize) -> Result<Vec<LoadedDetection>, String> {
    let mut ids: Vec<(u32, String)> = job.project.detections.iter()
        .filter(|row| row.source_idx == source_idx)
        .map(|row| (row.order, row.id.clone()))
        .collect();
    ids.sort();
    ids.into_iter()
        .map(|(_, id)| job.load_detection(&id).map_err(|e| e.to_string())?
            .ok_or_else(|| format!("detection {id} is not stored")))
        .collect()
}

/// A page the run could not finish: it is counted, it is **not** marked
/// examined - so a later run tries it again - and `page-done` still goes out, or
/// the interface would leave a `●` on it for the rest of the session.
///
/// The counter is moved by [`Job::mark_errored`] rather than by hand, because
/// the page will be attempted again and a counter incremented per attempt
/// climbs for ever over one corrupt file.
fn fail_page(
    emit: &dyn Fn(Event),
    run_id: &str,
    entry: &PlanEntry,
    page_id: &str,
    job: &mut Job,
    summary: &mut Summary,
) {
    summary.errored += 1;
    let _ = job.mark_errored(entry.source_idx);
    emit_page_done(emit, run_id, entry, page_id, job);
}

fn emit_page_done(
    emit: &dyn Fn(Event),
    run_id: &str,
    entry: &PlanEntry,
    page_id: &str,
    job: &Job,
) {
    emit(Event::PageDone {
        run_id: run_id.to_owned(),
        chapter_id: entry.chapter_id.clone(),
        page_id: page_id.to_owned(),
        page_index: entry.page_index as u32,
        page: Box::new(page_or_stub(entry, page_id, job)),
    });
}

/// The page a run's events carry, or the little that is known about it.
///
/// [`library::page_of`] answers `None` when the manifest cannot describe the
/// page - a `strip.order` position with no source behind it, which is a
/// manifest another writer has moved under the run. **Emitting nothing was the
/// wrong answer to that**, and in two ways the interface feels: a
/// `page-started` with no matching `page-done` leaves the page marked as
/// cleaning for the rest of the session, and a dropped `region-done` after
/// `regions_cleaned` had already been incremented let `run-finished` claim more
/// regions than the events ever reported. So the event goes out with what the
/// run itself knows - the page's id, its position, and no regions, which is
/// exactly the claim "the manifest cannot describe this page" amounts to.
fn page_or_stub(entry: &PlanEntry, page_id: &str, job: &Job) -> ApiPage {
    library::page_of(&entry.chapter_id, &job.project, entry.page_index).unwrap_or(ApiPage {
        id: page_id.to_owned(),
        chapter_id: entry.chapter_id.clone(),
        index: entry.page_index as u32,
        source_index: entry.source_idx,
        number: entry.page_index as u32 + 1,
        file: String::new(),
        source_sha: String::new(),
        width: 0,
        height: 0,
        status: "unclean",
        skip_reason: None,
        regions: Vec::new(),
        // A stub is a page the manifest cannot describe, so it has nothing to
        // count and nothing was loaded - `resident: true` with an empty list is
        // the honest reading of "no regions", and it is what stops the
        // interface asking for a window it will never get an answer to.
        region_count: 0,
        done_count: 0,
        review_count: 0,
        candidate_count: 0,
        resident: true,
        appearance: String::new(),
    })
}

/// Record where a job stopped, or that it did not stop.
///
/// `mark_interrupted(None)` is not a no-op and has to happen: a job that
/// records being interrupted with no way to say it no longer is would offer a
/// resume for ever.
fn close(path: &Path, job: Job, interrupted_at: Option<u32>) {
    // The closing save is refused like any other when another writer saved
    // since the run read the job, which the run only learns by trying: then
    // the job is read again, under the lock the walk still holds, and the
    // resume point recorded on what that writer saved.
    let Some(mut job) = current(path, job) else { return };
    if job.mark_interrupted(interrupted_at).is_err() && job.went_stale() {
        if let Some(mut fresh) = current(path, job) {
            let _ = fresh.mark_interrupted(interrupted_at);
        }
    }
}

/* ------------------------------------------------------------------ */
/* The active run                                                      */
/* ------------------------------------------------------------------ */

/// One run in flight.
///
/// Runs on different chapters go on side by side, up to [`MAX_RUNS`]; a run
/// asked for on a chapter one of them already walks answers `alreadyRunning`
/// with that run's id rather than starting a second over the same manifests.
/// The chapter file lock ([`lock_job`]) keeps two writers off one manifest
/// either way; this keeps two runs from racing each other through one queue.
struct Active {
    run_id: String,
    /// The chapter the run was started on, as `list_jobs` names it.
    chapter_id: String,
    /// Every chapter the run's queue walks: its own, and for a project scope
    /// the project's others. Another run on any of them is refused.
    chapters: Vec<String>,
    kind: RunKind,
    progress: Arc<RunProgress>,
    cancel: Arc<AtomicBool>,
    /// Raised when the worker has emitted `run-finished` and cleared itself.
    /// `cancelRun` waits on it, so that - as in the mock - the call does not
    /// answer before the event the caller is about to look for has gone out.
    finished: Arc<(Mutex<bool>, Condvar)>,
    /// The `reason` the run's `run-finished` carried, once it has gone out.
    ///
    /// `finished` says the worker has let go; this says what it said on the way
    /// out, and they are not the same question. `cancelRun` needs the second
    /// one: between `execute` returning and the slot being cleared there is a
    /// window in which the run is over, `run-finished` has already gone out
    /// saying `completed`, and the slot still holds it - and a cancel that
    /// arrived in that window used to set the flag on a dead run, wait, and
    /// then name it as cancelled to a caller whose event stream says otherwise.
    outcome: Arc<Mutex<Option<&'static str>>>,
}

fn active() -> &'static Mutex<Vec<Active>> {
    static ACTIVE: OnceLock<Mutex<Vec<Active>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Vec::new()))
}

/// The most runs in flight at once. Each local run opens its own ONNX
/// pipelines, so this bounds memory; the answer over it is `atCapacity`.
pub(crate) const MAX_RUNS: usize = 3;

/// What a run does, as `list_jobs` names it. An Auto run is named `clean`:
/// cleaning is where it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunKind {
    Detect,
    Clean,
    CloudClean,
}

impl RunKind {
    pub(crate) fn of(mode: RunMode) -> RunKind {
        match mode {
            RunMode::Detect => RunKind::Detect,
            RunMode::Clean | RunMode::Auto => RunKind::Clean,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RunKind::Detect => "detect",
            RunKind::Clean => "clean",
            RunKind::CloudClean => "cloudClean",
        }
    }
}

/// How far a run is, for `list_jobs`: pages queued, and pages whose
/// `page-done` has gone out.
#[derive(Debug, Default)]
pub(crate) struct RunProgress {
    done: AtomicU32,
    total: AtomicU32,
}

impl RunProgress {
    fn set_total(&self, total: usize) {
        self.total.store(u32::try_from(total).unwrap_or(u32::MAX), Ordering::Relaxed);
    }

    /// Counts a `page-done` on its way out.
    fn observe(&self, event: &Event) {
        if matches!(event, Event::PageDone { .. }) {
            self.done.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `(done, total)`, with `done` held to `total`: a page a cloud clean
    /// settles twice is still one page.
    fn read(&self) -> (u32, u32) {
        let total = self.total.load(Ordering::Relaxed);
        (self.done.load(Ordering::Relaxed).min(total), total)
    }
}

/// The answer a run asked for on `chapter_id`, over `chapters`, gets instead of
/// a slot, if any: the run already walking one of them, or `atCapacity`.
fn refusal(slots: &[Active], chapters: &[String]) -> Option<RunHandle> {
    if let Some(run) = slots.iter().find(|run| run.chapters.iter().any(|walked| chapters.contains(walked))) {
        return Some(RunHandle { run_id: Some(run.run_id.clone()), already_running: Some(true), ..RunHandle::default() });
    }
    (slots.len() >= MAX_RUNS).then(|| RunHandle {
        run_id: None, already_running: Some(true), at_capacity: Some(true), ..RunHandle::default()
    })
}

/// `backend-run-1`, `backend-run-2`, ….
///
/// **The prefix is load-bearing.** `subscribe` is a merge of this stream and
/// the mock's, and the mock mints `run-1`,
/// `run-2`, … from a counter of its own - `applyTool({tool: 'autoClean'})` is
/// still the mock's and starts a real mock run, so two schedulers can be live at
/// once on one stream while `editor.run` holds exactly one run id. "A run
/// belongs entirely to one implementation" is true and does not make the two
/// counters disjoint; a prefix does.
fn next_run_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("backend-run-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// How long `cancelRun` will wait for the worker to stop.
///
/// A bound rather than a promise: the worker checks for cancellation between
/// regions, and one region of one page is the longest it can be from noticing.
/// If that were ever to exceed this, answering late is better than never
/// answering - the run id is still correct and `run-finished` still arrives on
/// the channel.
const CANCEL_WAIT: Duration = Duration::from_secs(120);

/* ------------------------------------------------------------------ */
/* The seam's shapes                                                   */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedPage {
    pub chapter_id: String,
    pub page_id: String,
    pub page_index: u32,
}

/// `runClean` resolves with the **queue**, not the result.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunHandle {
    pub run_id: Option<String>,
    pub pages: Vec<QueuedPage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub already_running: Option<bool>,
    /// Set, with `alreadyRunning` and no run id, when [`MAX_RUNS`] runs are
    /// already in flight on other chapters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_capacity: Option<bool>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumedJob {
    pub project: ApiProject,
    pub chapter: ApiChapter,
    pub resumed_from: u32,
    pub run_id: Option<String>,
    pub pages: Vec<QueuedPage>,
    /// The step the resumed run takes, as the interrupted one captured it, so
    /// the editor can say whether it is detecting or cleaning.
    pub mode: &'static str,
}

fn queued(entries: &[PlanEntry]) -> Vec<QueuedPage> {
    entries
        .iter()
        .map(|entry| QueuedPage {
            chapter_id: entry.chapter_id.clone(),
            page_id: library::page_id(&entry.chapter_id, entry.page_index),
            page_index: entry.page_index as u32,
        })
        .collect()
}

/* ------------------------------------------------------------------ */
/* Starting one                                                        */
/* ------------------------------------------------------------------ */

/// Whether a run by this id still holds its slot.
pub(crate) fn is_running(run_id: &str) -> bool {
    active().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().any(|run| run.run_id == run_id)
}

/// The run walking this chapter, if one is.
pub(crate) fn chapter_run(chapter_id: &str) -> Option<String> {
    active().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter()
        .find(|run| run.chapters.iter().any(|walked| walked == chapter_id))
        .map(|run| run.run_id.clone())
}

/// Every run holding a slot, for `list_jobs`, in the order they started.
pub(crate) fn jobs() -> Vec<crate::jobs::JobView> {
    active().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().map(|run| {
        let (done, total) = run.progress.read();
        crate::jobs::JobView {
            run_id: run.run_id.clone(), kind: run.kind.as_str(), chapter_id: run.chapter_id.clone(), done, total,
        }
    }).collect()
}

/// Raise every run's cancel flag and answer how many there were. Waits for
/// none of them: the quit path polls [`jobs`] with a bound of its own, where
/// `cancel_run` would wait out [`CANCEL_WAIT`] per run.
pub(crate) fn cancel_all() -> usize {
    let guard = active().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for run in guard.iter() {
        run.cancel.store(true, Ordering::SeqCst);
    }
    guard.len()
}

/// Holds the slot while consent is spent and the pipeline is prepared. A
/// pre-start error drops it; starting the worker transfers release to Worker.
pub(crate) struct RunReservation {
    run_id: String,
    progress: Arc<RunProgress>,
    cancel: Arc<AtomicBool>,
    finished: Arc<(Mutex<bool>, Condvar)>,
    outcome: Arc<Mutex<Option<&'static str>>>,
    armed: bool,
}

impl RunReservation {
    /// The pages the run will walk, once it knows them.
    pub(crate) fn set_total(&self, pages: usize) {
        self.progress.set_total(pages);
    }
}

impl Drop for RunReservation {
    fn drop(&mut self) {
        if self.armed { release(&self.run_id, &self.finished); }
    }
}

/// Claim a slot for a run started on `chapter_id` whose queue walks
/// `chapters` (empty: that chapter alone), or the answer it gets instead
/// ([`refusal`]).
pub(crate) fn reserve_run(chapter_id: &str, chapters: &[String], kind: RunKind)
    -> Result<Result<RunReservation, RunHandle>, String> {
    let mut chapters = chapters.to_vec();
    if !chapters.iter().any(|walked| walked == chapter_id) { chapters.push(chapter_id.to_owned()); }
    let mut guard = active().lock().map_err(|e| e.to_string())?;
    if let Some(busy) = refusal(&guard, &chapters) {
        return Ok(Err(busy));
    }
    let reservation = RunReservation {
        run_id: next_run_id(), progress: Arc::new(RunProgress::default()),
        cancel: Arc::new(AtomicBool::new(false)),
        finished: Arc::new((Mutex::new(false), Condvar::new())),
        outcome: Arc::new(Mutex::new(None)), armed: true,
    };
    guard.push(Active {
        run_id: reservation.run_id.clone(), chapter_id: chapter_id.to_owned(), chapters, kind,
        progress: Arc::clone(&reservation.progress), cancel: Arc::clone(&reservation.cancel),
        finished: Arc::clone(&reservation.finished), outcome: Arc::clone(&reservation.outcome),
    });
    Ok(Ok(reservation))
}

/// Start a run whose work is not [`walk`] in the ordinary run slot: a batch
/// cloud clean (`docs/detect-clean.md` §3). It is one run like any other: it
/// answers `alreadyRunning` while another holds its chapter, `cancel_run`
/// reaches it, it ends with exactly one `run-finished` and the notice
/// [`report`] gives its summary, and the slot is handed back however it ends.
///
/// `body` owns the page events and every notice but the closing one; it reads
/// the cancel flag it is handed and answers with the run's summary.
pub(crate) fn start_reserved_in_slot<F>(
    mut reservation: RunReservation, chapter_id: &str, pages: Vec<QueuedPage>, body: F,
) -> RunHandle
where F: FnOnce(&str, &AtomicBool, &dyn Fn(Event)) -> Summary + Send + 'static,
{
    reservation.armed = false;
    reservation.set_total(pages.len());
    let run_id = reservation.run_id.clone();
    let cancel = Arc::clone(&reservation.cancel);
    let finished = Arc::clone(&reservation.finished);
    let outcome = Arc::clone(&reservation.outcome);
    let progress = Arc::clone(&reservation.progress);
    let handle = RunHandle { run_id: Some(run_id.clone()), pages, ..RunHandle::default() };
    let chapter_id = chapter_id.to_owned();
    std::thread::spawn(move || {
        let _release = Release { run_id: run_id.clone(), finished };
        run_body(&run_id, &chapter_id, &cancel, &outcome, body, &|event| {
            progress.observe(&event);
            events::emit(&event);
        });
    });
    handle
}

/// [`start_reserved_in_slot`]'s thread, apart from the slot so a test can drive it: the
/// body, with a panic contained as [`execute`] contains one, then the one
/// `run-finished`, the outcome `cancel_run` reads, and the closing notice.
pub(crate) fn run_body<F>(
    run_id: &str,
    chapter_id: &str,
    cancel: &AtomicBool,
    outcome: &Mutex<Option<&'static str>>,
    body: F,
    emit: &dyn Fn(Event),
) -> Summary
where
    F: FnOnce(&str, &AtomicBool, &dyn Fn(Event)) -> Summary,
{
    let summary = std::panic::catch_unwind(AssertUnwindSafe(|| body(run_id, cancel, emit)))
        .unwrap_or(Summary {
            reason: "cancelled",
            pages_queued: 0,
            pages_cleaned: 0,
            regions_cleaned: 0,
            next_page_index: None,
            errored: 0,
            regions_detected: 0,
        });
    emit(Event::RunFinished {
        run_id: run_id.to_owned(),
        chapter_id: chapter_id.to_owned(),
        reason: summary.reason,
        pages_queued: summary.pages_queued,
        pages_cleaned: summary.pages_cleaned,
        regions_cleaned: summary.regions_cleaned,
        next_page_index: summary.next_page_index,
    });
    if let Ok(mut recorded) = outcome.lock() {
        *recorded = Some(summary.reason);
    }
    report(&summary, emit);
    summary
}

/// `page-started` for a page a [`start_reserved_in_slot`] run is about to work on.
pub(crate) fn page_started_event(run_id: &str, chapter_id: &str, page_index: u32) -> Event {
    Event::PageStarted {
        run_id: run_id.to_owned(),
        chapter_id: chapter_id.to_owned(),
        page_id: library::page_id(chapter_id, page_index as usize),
        page_index,
    }
}

/// Finish a page whose stored detections a run cleaned outside [`walk`]: the
/// page is marked examined once none is left, as a Clean run marks it, and the
/// answer is its `page-done`, with the page as the manifest now holds it.
pub(crate) fn page_done_after_clean(run_id: &str, chapter_id: &str, job_path: &Path, page_index: u32) -> Event {
    let page_id = library::page_id(chapter_id, page_index as usize);
    // Without the lock, because another process holds the chapter, the page
    // is still answered as the manifest holds it; it is only not marked.
    let lock = lock_job(job_path).ok();
    let job = Job::open(job_path).ok();
    let source_idx = job.as_ref().and_then(|job| Library::resolve_page(&job.project, page_index as usize));
    let page = match (job, source_idx) {
        (Some(mut job), Some(source_idx)) => {
            if lock.is_some() && !has_detections(&job.project, source_idx) {
                let _ = job.mark_examined(source_idx);
            }
            let entry = PlanEntry {
                chapter_id: chapter_id.to_owned(),
                job_path: job_path.to_path_buf(),
                page_index: page_index as usize,
                source_idx,
            };
            page_or_stub(&entry, &page_id, &job)
        }
        _ => ApiPage {
            id: page_id.clone(),
            chapter_id: chapter_id.to_owned(),
            index: page_index,
            source_index: 0,
            number: page_index + 1,
            file: String::new(),
            source_sha: String::new(),
            width: 0,
            height: 0,
            status: "unclean",
            skip_reason: None,
            regions: Vec::new(),
            region_count: 0,
            done_count: 0,
            review_count: 0,
            candidate_count: 0,
            resident: true,
            appearance: String::new(),
        },
    };
    Event::PageDone {
        run_id: run_id.to_owned(),
        chapter_id: chapter_id.to_owned(),
        page_id,
        page_index,
        page: Box::new(page),
    }
}

/// A cloud clean's local first pass (`docs/detect-clean.md` §3): each stored
/// detection among `ids` whose pick is `fill` or `solid` is cleaned on this
/// computer at rung 0 alone, and the result is kept only when the quality
/// metric accepts it, a solid fill included. A kept result replaces its
/// detection in one write, as any clean does, and is final. Everything else is
/// left exactly as it was, for the cloud. Answers the ids it cleaned.
///
/// Rung 0 is arithmetic on this computer: no model opens and nothing is sent.
/// A page whose detections are all cleaned here is finished, as a Clean run
/// finishes one.
pub(crate) fn clean_fill_rung_first(job_path: &Path, ids: &[String], cancel: &AtomicBool)
    -> Result<Vec<String>, String> {
    // No path is ever opened: rung 0 is the ceiling, and the models are only
    // asked for by a rung above it.
    let mut pipeline = Pipeline::open_for_cleaning(Path::new(""), Preference::default(), None);
    pipeline.gate_solid_fill = true;
    clean_picks_here(job_path, ids, &["fill", "solid"], Engine::Fill, pipeline, cancel)
}

/// Write a clean's picks onto stored detections before a cloud clean plans
/// them: each of `ids` gets the pick a region found now would be saved with
/// ([`stored_pick`] over its fit's route, `ceiling` and the pick for its
/// side of a balloon). A side `repick` names no pick for keeps each region's
/// saved pick. The plan, its LaMa pass here and its mixed pass then all read
/// the one pick the user chose for this clean. Answers how many changed;
/// nothing is written when none did.
pub(crate) fn repick_detections(job_path: &Path, ids: &[String], repick: Repick)
    -> Result<usize, String> {
    let _lock = lock_job(job_path)?;
    let mut job = Job::open(job_path).map_err(|e| e.to_string())?;
    let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let ceiling = repick.ceiling;
    let mut changed = 0;
    for row in job.project.detections.iter_mut().filter(|row| wanted.contains(row.id.as_str())) {
        let Some(pick) = (if row.inside { repick.bubble } else { repick.outside }) else { continue };
        let name = row.fit.as_ref().map_or_else(|| pick_name(pick), |fit| stored_pick(fit.route, ceiling, Some(pick)));
        if row.pick != name {
            row.pick = name.to_owned();
            changed += 1;
        }
    }
    if changed > 0 {
        job.flush().map_err(|e| e.to_string())?;
        library::invalidate_manifest_cache(job_path);
    }
    Ok(changed)
}

/// Clean on this computer each stored detection among `ids` whose pick is one
/// of `picks`, under `ceiling`, through `pipeline`, and commit what is kept.
/// One region at a time, so `cancel` is heard between regions; what was
/// committed before it stays. Answers the ids it cleaned.
fn clean_picks_here(job_path: &Path, ids: &[String], picks: &[&str], ceiling: Engine, mut pipeline: Pipeline,
    cancel: &AtomicBool) -> Result<Vec<String>, String> {
    let _lock = lock_job(job_path)?;
    let mut job = Job::open(job_path).map_err(|e| e.to_string())?;
    let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut pages: std::collections::BTreeMap<usize, Vec<(u32, String)>> = std::collections::BTreeMap::new();
    for row in &job.project.detections {
        if !wanted.contains(row.id.as_str()) || !picks.contains(&row.pick.as_str()) {
            continue;
        }
        if let Some(placement) = job.project.strip.order.iter().position(|source| *source == row.source_idx) {
            pages.entry(placement).or_default().push((row.order, row.id.clone()));
        }
    }
    if pages.is_empty() {
        return Ok(Vec::new());
    }
    let geometry = survey_job(&mut job, &|_| {});
    let mut cleaned = Vec::new();
    for (placement, mut rows) in pages {
        if cancel.load(Ordering::SeqCst) { break; }
        rows.sort();
        let Some(source_idx) = job.project.strip.order.get(placement).copied() else { continue };
        let Some(bytes) = job.source_path(source_idx).and_then(|path| std::fs::read(path).ok()) else { continue };
        let context = PageContext {
            strip: &geometry.strip,
            joins: &geometry.survey.joins,
            segments: &geometry.survey.segments,
            placement,
            sources: &geometry.sources,
        };
        // Each region is cleaned from the page's own pixels, so one at a time
        // cleans it as the whole page at once would.
        for (_, id) in &rows {
            if cancel.load(Ordering::SeqCst) { break; }
            let Some(loaded) = job.load_detection(id).ok().flatten() else { continue };
            let Ok(outcome) = pipeline.clean_detections("", &bytes, ceiling, &context, std::slice::from_ref(&loaded))
                else { continue };
            for (id, region) in outcome.regions {
                if let RegionOutcome::Cleaned(patch, review_state) = region {
                    let mut patch = *patch;
                    patch.provenance.mask_sha256 = mask_digest(&patch.mask);
                    if job.complete_detection(&id, &patch, review_state).map_err(|e| e.to_string())? {
                        cleaned.push(id);
                    }
                }
            }
        }
        if !has_detections(&job.project, source_idx) {
            let _ = job.mark_examined(source_idx);
        }
    }
    library::invalidate_manifest_cache(job_path);
    Ok(cleaned)
}

/// A detecting run with a stage on the cloud GPU uses Ogkalu Full there, so it
/// never holds the Small profile: Small has no cloud twin and Full replaces
/// it. CTD and the text reader are welcome: they are light, give the same
/// results anywhere, and run on this computer beside the cloud stages
/// (`pipelines.js#runDetection` sends CTD + Full + SAM-TS-L + the reader). A
/// clean-only run opens no detector.
fn cloud_detect_refusal(mode: RunMode, targets: crate::inference::run_analysis::AnalysisTargets,
    models: DetectorModels) -> Option<&'static str> {
    (mode != RunMode::Clean && !targets.capabilities(models.rt_full, models.sam).is_empty() && models.rt_small)
        .then_some("cloud_detect_small_unsupported")
}

/// Why a detecting run that asked for the text reader will run without any,
/// or `None` when it has one or did not ask. Hayai is the reader; manga-ocr
/// stands in for Japanese when Hayai is not installed, and when that fails too
/// [`Pipeline::with_selection`] already says so with manga-ocr's own reason.
/// What is left is the case nothing said: Hayai missing and no manga-ocr
/// fallback asked (no Japanese cleaned, or the All text policy), on this
/// computer or beside a cloud run alike.
fn reader_gap(selection: RunSelection, hayai_missing: Option<String>) -> Option<String> {
    if !selection.reader || selection.ocr_rescue { return None; }
    hayai_missing
}

/// Why the Hayai reader cannot be opened from `models`, or `None` when its
/// files are all there.
fn unavailable_hayai_reason(models: &Path) -> Option<String> {
    let missing: Vec<&str> = HAYAI_MODELS.iter().copied()
        .filter(|name| !std::fs::metadata(models.join(name)).is_ok_and(|m| m.len() > 0))
        .collect();
    (!missing.is_empty()).then(|| format!("Hayai OCR is not installed: missing {}", missing.join(", ")))
}

/// Whether a run's cloud stages may even be looked at. Refused before anything
/// else, including the no-op answers: a grant for a run that will not send is a
/// caller that disagrees with itself, a cloud run across chapters has no single
/// consent to spend, and a cloud run without consent does not start.
pub(crate) fn cloud_refusal(capabilities: &[String], scope: &str, has_grant: bool) -> Option<&'static str> {
    match (capabilities.is_empty(), has_grant) {
        (true, true) => Some("cloud_run_grant_mismatch: capabilities"),
        (true, false) => None,
        (false, _) if scope == "project" => Some("cloud_run_scope_unsupported"),
        (false, false) => Some("cloud_run_grant_required"),
        (false, true) => None,
    }
}

/// The entries of a queue a run will read with a detector. Each job is opened
/// once, under its lock, to ask whether the page has detections waiting.
fn pages_to_detect(entries: &[PlanEntry], mode: RunMode) -> Vec<PlanEntry> {
    match mode {
        RunMode::Detect => entries.to_vec(),
        RunMode::Clean => Vec::new(),
        RunMode::Auto => {
            let mut opened: Option<(&Path, Option<Job>)> = None;
            entries.iter().filter(|entry| {
                // A read, so without the lock (see `lock_job`).
                if opened.as_ref().is_none_or(|(path, _)| *path != entry.job_path.as_path()) {
                    opened = Some((entry.job_path.as_path(), Job::open(&entry.job_path).ok()));
                }
                !opened.as_ref().and_then(|(_, job)| job.as_ref())
                    .is_some_and(|job| has_detections(&job.project, entry.source_idx))
            }).cloned().collect()
        }
    }
}

/// Where a run's cloud-capable stages go, and the consent that lets them.
#[derive(Default)]
pub(crate) struct CloudStages {
    pub targets: crate::inference::run_analysis::AnalysisTargets,
    pub grant: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn start_with_models(
    app: &tauri::AppHandle,
    scope: &str,
    chapter_id: &str,
    page_index: Option<u32>,
    engine_ceiling: Option<String>,
    picks: Picks,
    outside: OutsideText,
    bubble_color: Option<[u8; 3]>,
    mask_padding: u32,
    selection: RunSelection,
    detection_snapshot: Option<serde_json::Value>,
    detection_models: DetectorModels,
    all_text: bool,
    cloud: CloudStages,
    mode: RunMode,
    area: Option<DetectArea>,
) -> Result<RunHandle, String> {
    // An area is one page's, and it is looked at, not cleaned.
    if area.is_some() && (mode != RunMode::Detect || scope != "page") {
        return Err("detect_area_scope_unsupported".to_owned());
    }
    if let Some(refusal) = cloud_detect_refusal(mode, cloud.targets, detection_models) {
        return Err(refusal.into());
    }
    // A clean detects nothing, so it has no cloud stage to consent to whatever
    // the targets say, and a grant handed to one is refused as a grant for a
    // run that will not send.
    let cloud_capabilities = if mode == RunMode::Clean { Vec::new() } else {
        cloud.targets.capabilities(detection_models.rt_full, detection_models.sam)
    };
    if let Some(refusal) = cloud_refusal(&cloud_capabilities, scope, cloud.grant.is_some()) {
        return Err(refusal.into());
    }
    // The language choices are the gate's, and a clean asks no gate: what it
    // cleans was gated when it was detected.
    if let Some(key) = selection.empty_notice().filter(|_| mode != RunMode::Clean) {
        events::notice(key, serde_json::json!({}), "warn");
        return Ok(RunHandle::default());
    }
    // Before the library is indexed: a chapter already being walked, or no
    // slot free, is answered without it. The reservation below checks again
    // against every chapter the queue turns out to walk.
    {
        let guard = active().lock().map_err(|e| e.to_string())?;
        if let Some(busy) = refusal(&guard, &[chapter_id.to_owned()]) {
            return Ok(busy);
        }
    }

    let library = Library::for_app(app).map_err(|e| e.to_string())?;
    // A page is always there to look at again, for the cloud or for an area:
    // neither replaces what it holds.
    let entries = if mode == RunMode::Detect && (!cloud_capabilities.is_empty() || area.is_some()) {
        plan_cloud_detection(&library, scope, chapter_id, page_index)
    } else {
        plan_for(&library, scope, chapter_id, page_index, mode)
    }.map_err(|e| e.to_string())?;
    if entries.is_empty() {
        events::notice(
            if mode == RunMode::Clean { "notice.run.nothingDetected" } else { "notice.run.nothingInScope" },
            serde_json::json!({}),
            "warn",
        );
        return Ok(RunHandle::default());
    }
    let walked: Vec<String> = entries.iter().map(|entry| entry.chapter_id.clone())
        .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let mut reservation = match reserve_run(chapter_id, &walked, RunKind::of(mode))? {
        Ok(reservation) => reservation,
        Err(busy) => return Ok(busy),
    };
    reservation.set_total(entries.len());
    // The pages this run will read with a detector. An Auto page with
    // detections waiting is cleaned from them and never sent anywhere.
    let to_detect = pages_to_detect(&entries, mode);
    // The grant is spent here, before a single model loads, and only for the
    // pages this run will actually detect. An Auto run whose every page has
    // detections waiting sends nothing and spends nothing.
    let remote = if cloud_capabilities.is_empty() || to_detect.is_empty() { None } else {
        let pages: Vec<u32> = to_detect.iter().map(|entry| entry.page_index as u32).collect();
        Some(crate::inference::run_analysis::open_for_run(
            app, chapter_id, &pages, &cloud_capabilities, cloud.grant.as_deref())?)
    };

    // The ONNX Runtime is downloaded after install, so "is it there" is a
    // question with an answer that changes over a session's life. The four
    // outcomes are told apart because their remedies are, and the catalogue
    // already has a key for each - `diagnostics::onnx_runtime` reports the same
    // four to the About window.
    let app_data = {
        use tauri::Manager;
        app.path().app_data_dir().ok()
    };
    if requires_runtime_before_start(mode) {
        if let Err(error) = cleaner_core::runtime::find(app_data.as_deref())
            .and_then(|path| cleaner_core::runtime::load(&path)) {
            use cleaner_core::runtime::LoadError;
            events::notice(
                match error {
                    LoadError::NotFound { .. } => "diagnostics.runtime.missing",
                    LoadError::Quarantined { .. } => "diagnostics.runtime.quarantined",
                    LoadError::Refused { .. } => "diagnostics.runtime.refused",
                    LoadError::MissingDependency { .. } => "diagnostics.runtime.missingDependency",
                    _ => "diagnostics.runtime.unloadable",
                },
                serde_json::json!({}),
                "warn",
            );
            return Ok(RunHandle::default());
        }
    }

    // The weights, and the one failure this reports as a notice rather than as
    // an error. A run that cannot start for want of the model files used to
    // answer `Err`, and the interface had nowhere to put it - the doc comment
    // on `run_clean` said so. Now that Settings › Models can *fetch* them there
    // is somewhere for the user to go, so the gap is closed the same way the
    // missing-runtime gap already was: a warning that names the remedy, and an
    // empty handle rather than a rejected promise.
    // A clean needs none of the detection weights, so it does not ask for
    // them: the directory is the one a detecting run would use if there is
    // one, and otherwise the first place weights are looked for, where rung 2
    // finds its own or declines.
    let models = if mode == RunMode::Clean {
        selected_model_dir(app_data.as_deref(), detection_models, all_text)
            .or_else(|| model_search_paths(app_data.as_deref()).into_iter().next())
    } else {
        selected_model_dir(app_data.as_deref(), detection_models, all_text)
    };
    let Some(models) = models else {
        events::notice("notice.run.modelsMissing", serde_json::json!({}), "warn");
        return Ok(RunHandle::default());
    };
    if mode != RunMode::Clean && detection_models.rt_full && !cloud.targets.rt_full && app_data.as_deref()
        .and_then(crate::model_workflows::full_rt_path).is_none() {
        events::notice("notice.run.modelsMissing", serde_json::json!({ "model": crate::models::RT_FULL_NAME }), "warn");
        return Ok(RunHandle::default());
    }
    if mode != RunMode::Clean && detection_models.sam && !cloud.targets.sam && app_data.as_deref()
        .and_then(crate::model_workflows::graph_dir)
        .is_none_or(|dir| crate::model_workflows::verify_graphs(&dir).is_err()) {
        events::notice("notice.run.modelsMissing", serde_json::json!({ "model": crate::models::SAM_TS_NAME }), "warn");
        return Ok(RunHandle::default());
    }
    // Read before the pipeline is opened, because the pipeline's sessions are
    // built here and the accelerator is a property of a session rather than of
    // a run. A setting read afterwards would take effect one run late.
    let settings = crate::settings::read(app).unwrap_or(serde_json::Value::Null);
    // The same notice as the `model_dir` miss above, and it is reached when a
    // required file went away between that check and this one. Matched on the
    // variant rather than on the message: a second failure mode added
    // to `OpenError` will fail to compile here rather than falling into the
    // notice or out of it by the shape of its wording.
    let opened = if mode == RunMode::Clean {
        Ok(Pipeline::open_for_cleaning(&models, preference_from(&settings), app_data.clone()))
    } else {
        Pipeline::open_selected(&models, preference_from(&settings), detection_models, all_text,
            app_data.clone())
    };
    let pipeline = match opened {
        Ok(pipeline) => pipeline.with_remote(remote).with_model_preferences(&settings)?
            .with_picks(picks)
            .with_outside(if all_text { OutsideText::Clean } else { outside })
            .with_selection(selection)
            .with_bubble_color(bubble_color)
            .with_mask_padding(mask_padding)
            .with_area(area),
        Err(OpenError::MissingModel { .. }) => {
            events::notice("notice.run.modelsMissing", serde_json::json!({}), "warn");
            return Ok(RunHandle::default());
        }
    };
    // A reader asked for and not there is said, never dropped quietly. A
    // clean reads nothing, so it is not told.
    if mode != RunMode::Clean {
        if let Some(reason) = reader_gap(selection, unavailable_hayai_reason(&models)) {
            events::notice("notice.run.ocrRescueUnavailable", serde_json::json!({ "reason": reason }), "warn");
        }
    }

    let stored =
        settings.get("engineCeiling").and_then(|value| value.as_str()).map(|s| s.to_owned());
    let ceiling = effective_ceiling(engine_ceiling.as_deref(), stored.as_deref());

    reservation.armed = false;
    let run_id = reservation.run_id.clone();
    let handle = RunHandle { run_id: Some(run_id.clone()), pages: queued(&entries), ..RunHandle::default() };
    let progress = Arc::clone(&reservation.progress);
    let worker = Worker { run_id, chapter_id: chapter_id.to_owned(), mode,
        cancel: Arc::clone(&reservation.cancel), finished: Arc::clone(&reservation.finished),
        outcome: Arc::clone(&reservation.outcome) };
    let settings_snapshot = run_settings_snapshot(
        &settings, detection_snapshot, &pipeline, engine_ceiling.as_deref(), scope, page_index, mode,
    );
    // Which exact models this run detects with, for anyone reading the log:
    // SAM-TS-L by checkpoint and revision, never a bare "SAM" or "Koharu".
    if mode != RunMode::Clean {
        for use_ in pipeline.model_uses() {
            eprintln!("manga-cleaner: detection model: {}", use_.describe());
        }
    }
    let mut pipeline = pipeline;
    if let Some(remote) = pipeline.remote.as_mut() { remote.set_cancel(Arc::clone(&worker.cancel)); }
    std::thread::spawn(move || {
        let mut cleaner = pipeline;
        // The settings a job ran under, recorded on the job it ran over. Under
        // the job's own lock, because this is a read-modify-write like any
        // other and the run is about to take the same lock for the run itself.
        for path in
            entries.iter().map(|entry| &entry.job_path).collect::<std::collections::BTreeSet<_>>()
        {
            // A chapter another process holds is not waited on twice: the walk
            // finds it busy too and says so.
            let Ok(_lock) = lock_job(path) else { continue };
            if let Ok(mut job) = Job::open(path) {
                job.project.settings = settings_snapshot.clone();
                let _ = job.flush();
            }
        }
        // Before the walk, so the cloud GPU starts on page one while the run
        // opens its first job and its local models. Only the pages the walk
        // will detect: a page it cleans from stored detections never takes an
        // answer, and one sent for it would be analysis nobody reads.
        // Detect always uses its cloud answer, so it can prefetch. Auto's work
        // depends on the manifest when the walk reaches each page; analyzing
        // it there prevents a newly stored detection from wasting an upload.
        if mode == RunMode::Detect { cleaner.prefetch_remote(&to_detect); }
        worker.run(&entries, &mut cleaner, ceiling, &|event| {
            progress.observe(&event);
            events::emit(&event);
        });
    });
    Ok(handle)
}

fn requires_runtime_before_start(mode: RunMode) -> bool {
    mode != RunMode::Clean
}

fn run_settings_snapshot(
    settings: &serde_json::Value,
    detection_snapshot: Option<serde_json::Value>,
    pipeline: &Pipeline,
    engine_ceiling: Option<&str>,
    scope: &str,
    page_index: Option<u32>,
    mode: RunMode,
) -> serde_json::Value {
    let mut settings_snapshot = settings.clone();
    if !settings_snapshot.is_object() { settings_snapshot = serde_json::json!({}); }
    if let Some(snapshot) = detection_snapshot {
        settings_snapshot["runDetection"] = snapshot;
    }
    // The reader the run actually has, Hayai or manga-ocr: a resume asks for
    // what ran, not for what was asked and missing.
    settings_snapshot["runOcrRescue"] = serde_json::json!(pipeline.reads || pipeline.selection.ocr_rescue);
    settings_snapshot["runEngineCeiling"] = serde_json::json!(engine_ceiling);
    settings_snapshot["runScope"] = serde_json::json!(scope);
    settings_snapshot["runPageIndex"] = serde_json::json!(page_index);
    settings_snapshot["runMode"] = serde_json::json!(mode.as_str());
    settings_snapshot["runGeometryPolicy"] = serde_json::json!("legacy");
    settings_snapshot["runTextPolicy"] = serde_json::json!(if pipeline.all_text { "all_text" } else { "legacy_gate" });
    settings_snapshot["runAnalysisTargets"] = crate::inference::run_analysis::AnalysisTargets {
        rt_full: pipeline.remote_rt(), sam: pipeline.remote_sam(),
    }.to_value();
    settings_snapshot["runDetectorModels"] = serde_json::json!([
        pipeline.detection_models.ctd.then_some("ctd"),
        pipeline.detection_models.rt_small.then_some("rtSmall"),
        pipeline.detection_models.rt_full.then_some("rtFull"),
        pipeline.detection_models.sam.then_some("samTs"),
    ].into_iter().flatten().collect::<Vec<_>>());
    // Diagnostic only, never read back: the checkpoint, pinned revision, place
    // and spatial input of each model the run detects with.
    settings_snapshot["runDetectionModelsDescribed"] =
        serde_json::json!(text_groups::describe_models(&pipeline.model_uses()));
    settings_snapshot["runBubbleEngine"] = serde_json::json!(pick_name(pipeline.picks.unwrap_or_default().bubble));
    settings_snapshot["runOutsideEngine"] = serde_json::json!(pick_name(pipeline.picks.unwrap_or_default().outside));
    settings_snapshot["runOutsideBubbles"] = serde_json::json!(if pipeline.outside == OutsideText::Clean { "clean" } else { "review" });
    settings_snapshot["runMaskPadding"] = serde_json::json!(pipeline.mask_padding);
    // Written every run, so an area never outlives the run it was for: a
    // resume of a page Detect that read an older run's area would be held to it.
    settings_snapshot["runArea"] = pipeline.area.map_or(serde_json::Value::Null, DetectArea::to_value);
    if let Some(color) = pipeline.bubble_color {
        settings_snapshot["runBubbleColor"] = serde_json::json!(format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]));
    } else {
        settings_snapshot["runBubbleColor"] = serde_json::Value::Null;
    }
    settings_snapshot
}

/// The handles the thread that owns a run needs to finish it and hand the slot
/// back.
struct Worker {
    run_id: String,
    chapter_id: String,
    /// What each page's work is. See [`walk`].
    mode: RunMode,
    cancel: Arc<AtomicBool>,
    finished: Arc<(Mutex<bool>, Condvar)>,
    outcome: Arc<Mutex<Option<&'static str>>>,
}

impl Worker {
    /// Run to completion, and hand the slot back **whatever happens on the
    /// way**.
    ///
    /// The release is a guard rather than a line at the end, because the exits
    /// are not all visible here: [`execute`] contains a panic in the pipeline,
    /// but a panic in a *sink* - a subscriber that throws on the way out -
    /// unwinds through this frame, and a `release` that only ran on the happy
    /// path would leave `active()` occupied for the life of the session. Every
    /// later `runClean` would answer `alreadyRunning` for a run that has
    /// stopped, and every `cancelRun` would wait the full [`CANCEL_WAIT`] and
    /// then name it.
    fn run(
        &self,
        entries: &[PlanEntry],
        cleaner: &mut dyn Cleaner,
        ceiling: Engine,
        emit: &dyn Fn(Event),
    ) -> Summary {
        let _release = Release { run_id: self.run_id.clone(), finished: Arc::clone(&self.finished) };
        let summary = execute_as(
            self.mode,
            &self.run_id,
            &self.chapter_id,
            entries,
            cleaner,
            ceiling,
            &self.cancel,
            emit,
        );
        // Recorded before the slot is released, so a `cancelRun` woken by the
        // release finds the reason the run actually ended on.
        if let Ok(mut recorded) = self.outcome.lock() {
            *recorded = Some(summary.reason);
        }
        // An area that held nothing new is not an empty chapter: the page may
        // hold every mask it had.
        if cleaner.area_only() && summary.reason == "completed" && summary.regions_detected == 0
            && summary.errored == 0 {
            emit(events::notice_event("notice.run.nothingNewHere", serde_json::json!({}), "info"));
        } else {
            report(&summary, emit);
        }
        summary
    }
}

/// Hands the run's slot back when it goes. See [`Worker::run`].
struct Release {
    run_id: String,
    finished: Arc<(Mutex<bool>, Condvar)>,
}

impl Drop for Release {
    fn drop(&mut self) {
        release(&self.run_id, &self.finished);
    }
}

/// The notice a finished run leaves on the stack. The mock's `onFinished`,
/// key for key.
///
/// Emitted through the run's own sink rather than through the global registry,
/// so that the notice a run ends on is one of the things the scheduler's tests
/// can see. In the window the sink is `events::emit`, which is where it went
/// before.
/// **A run that failed pages never reports an empty result.** The branch used
/// to be `regions_cleaned == 0`, and every page of a run failing leaves that at
/// zero - so the closing word on a chapter no page of which could be cleaned
/// was "no text found, nothing to clean", which is the field saying the
/// opposite of what happened. It is the same class of lie the region writer's
/// `failed_here` was fixed for, one scope further out: a counter that reads
/// zero because nothing was found and a counter that reads zero because
/// everything broke are different runs, and `errored` is what tells them apart.
///
/// The two are **not** exclusive. A chapter can clean forty pages and fail
/// four, and a user told only the first half would go looking for the four
/// pages in review, where they are not. So the success notice goes out on its
/// own terms and the failure notice goes out after it, last, because the stack
/// reads bottom-up and the thing that needs acting on should not be the thing
/// that scrolls away.
fn report(summary: &Summary, emit: &dyn Fn(Event)) {
    if summary.reason == "cancelled" {
        emit(events::notice_event("notice.run.cancelled", serde_json::json!({}), "warn"));
        return;
    }
    if summary.regions_cleaned > 0 {
        emit(events::notice_event(
            "notice.run.finished",
            serde_json::json!({
                "pages": summary.pages_cleaned,
                "regions": summary.regions_cleaned,
            }),
            "info",
        ));
    } else if summary.regions_detected > 0 {
        // Detect's own closing word. `finished` would say regions were
        // cleaned, and `emptyResult` that none were found: both false.
        emit(events::notice_event(
            "notice.run.detected",
            serde_json::json!({
                "pages": summary.pages_cleaned,
                "regions": summary.regions_detected,
            }),
            "info",
        ));
    } else if summary.errored == 0 {
        // Nothing cleaned and nothing broken, which is the one reading of a
        // zero this key was ever true for.
        emit(events::notice_event(
            "notice.chapter.emptyResult",
            serde_json::json!({ "regions": 0, "pages": summary.pages_cleaned }),
            "warn",
        ));
    }
    if summary.errored > 0 {
        emit(events::notice_event(
            "notice.run.pagesFailed",
            serde_json::json!({ "pages": summary.errored }),
            "warn",
        ));
    }
}

/// Clear this run's slot and wake anything waiting on it. Only its own: the
/// other runs in flight keep theirs.
///
/// Through a poisoned lock, both times: this is the call that has to happen for
/// the *next* run to be startable at all, and a slot left occupied by a panic
/// somewhere else is the failure it exists to prevent.
fn release(run_id: &str, finished: &Arc<(Mutex<bool>, Condvar)>) {
    active().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).retain(|run| run.run_id != run_id);
    let (lock, condvar) = &**finished;
    *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
    condvar.notify_all();
}

struct CapturedRunPolicy {
    selection: RunSelection,
    detection: Option<serde_json::Value>,
    detector_models: DetectorModels,
    all_text: bool,
    engine_ceiling: Option<String>,
    scope: String,
    page_index: Option<u32>,
    picks: Picks,
    outside: OutsideText,
    bubble_color: Option<[u8; 3]>,
    /// Absent from a snapshot written before the Mask padding control: 0.
    mask_padding: u32,
    analysis_targets: crate::inference::run_analysis::AnalysisTargets,
    /// Absent from a snapshot written before the split, which was a one-pass
    /// run: Auto.
    mode: RunMode,
    /// The area an interrupted Detect was held to. Resumed as a page Detect
    /// it would replace every detection the page holds.
    area: Option<DetectArea>,
}

fn captured_run_policy(snapshot: &serde_json::Value) -> Result<CapturedRunPolicy, String> {
    if snapshot.get("runGeometryPolicy").and_then(|v| v.as_str())
        .is_some_and(|mode| mode != "legacy")
        || snapshot.get("runTextPolicy").and_then(|v| v.as_str())
            .is_some_and(|policy| !matches!(policy, "legacy_gate" | "all_text"))
    {
        return Err("This interrupted workflow cannot be resumed as legacy cleaning".into());
    }
    let detection = snapshot.get("runDetection").filter(|value| !value.is_null()).cloned();
    let all_text = snapshot.get("runTextPolicy").and_then(|v| v.as_str()) == Some("all_text");
    let rescue = snapshot.get("runOcrRescue").and_then(|value| value.as_bool());
    let selection = if all_text { RunSelection::all_text(rescue) } else { RunSelection::from_args(detection.as_ref(), rescue)? };
    let scope = snapshot.get("runScope").and_then(|value| value.as_str()).unwrap_or("chapter");
    if !matches!(scope, "chapter" | "project" | "page") {
        return Err("This interrupted workflow has an unknown run scope".into());
    }
    Ok(CapturedRunPolicy {
        selection,
        detection,
        detector_models: DetectorModels::from_args(snapshot.get("runDetectorModels"))?,
        all_text,
        engine_ceiling: snapshot.get("runEngineCeiling")
            .and_then(|value| value.as_str())
            .or_else(|| snapshot.get("engineCeiling").and_then(|value| value.as_str()))
            .map(str::to_owned),
        scope: scope.to_owned(),
        page_index: snapshot.get("runPageIndex")
            .and_then(|value| value.as_u64()).and_then(|value| u32::try_from(value).ok()),
        picks: Picks::from_args(
            snapshot.get("runBubbleEngine").and_then(|value| value.as_str()),
            snapshot.get("runOutsideEngine").and_then(|value| value.as_str()),
        ),
        outside: OutsideText::from_arg(
            snapshot.get("runOutsideBubbles").and_then(|value| value.as_str()),
        ),
        bubble_color: parse_color_hex(snapshot.get("runBubbleColor").and_then(|value| value.as_str())),
        mask_padding: snapshot.get("runMaskPadding").and_then(|value| value.as_u64())
            .map_or(0, |value| value.min(u64::from(cleaner_core::constants::MAX_MASK_PADDING)) as u32),
        // A snapshot an older build wrote may hold the stages apart; that
        // settles on this computer rather than failing the resume.
        analysis_targets: crate::inference::run_analysis::AnalysisTargets::from_stored(
            snapshot.get("runAnalysisTargets"))?,
        mode: RunMode::from_arg(snapshot.get("runMode").and_then(|value| value.as_str()))?,
        area: DetectArea::from_arg(snapshot.get("runArea"))?,
    })
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

/// `runClean`. Resolves with the queue; every result arrives on the channel.
///
/// `async`, with the body off the invoke handler's thread, for the reason
/// [`crate::library::blocking`] gives in full: a sync command is expanded
/// through `body_blocking` and runs *inline* on the thread the invoke arrives
/// on, which here would hold the webview through a whole-library index read and
/// three ONNX session opens.
///
/// **Both preconditions are now notices.** A missing runtime has always been
/// one - `diagnostics.runtime.*` says exactly that - and missing *weights*
/// used to be an `Err` the interface had nowhere to put. Settings › Models
/// can fetch both now, so `notice.run.modelsMissing` names that remedy and
/// `start` answers with an empty handle rather than a rejected promise.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn run_clean(
    app: tauri::AppHandle,
    scope: Option<String>,
    chapter_id: String,
    page_index: Option<u32>,
    engine_ceiling: Option<String>,
    bubble_engine: Option<String>,
    outside_engine: Option<String>,
    outside_bubbles: Option<String>,
    bubble_color: Option<String>,
    mask_padding_px: Option<u32>,
    detection: Option<serde_json::Value>,
    detector_models: Option<serde_json::Value>,
    geometry_policy: Option<String>,
    text_policy: Option<String>,
    ocr_rescue: Option<bool>,
    analysis_targets: Option<serde_json::Value>,
    cloud_grant: Option<String>,
    mode: Option<String>,
    area: Option<serde_json::Value>,
) -> Result<RunHandle, String> {
    crate::library::blocking(move || {
        let mode = RunMode::from_arg(mode.as_deref())?;
        let area = DetectArea::from_arg(area.as_ref())?;
        if !matches!(geometry_policy.as_deref(), None | Some("legacy")) {
            return Err("Text-shaped cleaning requires the prepared mask review workflow".to_owned());
        }
        if !matches!(text_policy.as_deref(), None | Some("legacy_gate") | Some("all_text")) {
            return Err("Unsupported text policy".to_owned());
        }
        let all_text = text_policy.as_deref() == Some("all_text");
        let selection = if all_text { RunSelection::all_text(ocr_rescue) } else { RunSelection::from_args(detection.as_ref(), ocr_rescue)? };
        let selected_models = DetectorModels::from_args(detector_models.as_ref())?;
        let picks = Picks::from_args(bubble_engine.as_deref(), outside_engine.as_deref());
        let outside = OutsideText::from_arg(outside_bubbles.as_deref());
        let parsed_color = parse_color_hex(bubble_color.as_deref());
        let mask_padding = mask_padding_px.unwrap_or(0);
        if mask_padding > cleaner_core::constants::MAX_MASK_PADDING {
            return Err("mask_padding_invalid".to_owned());
        }
        // The call names where detection runs, both stages in one place, and a
        // split is refused. An older caller that does not name it is held to
        // the saved setting, so a cloud choice is never ignored; a split an
        // older build stored settles on this computer.
        let targets = match analysis_targets.as_ref() {
            Some(value) => crate::inference::run_analysis::AnalysisTargets::from_value(Some(value))?,
            None => {
                let settings = crate::settings::read(&app).unwrap_or(serde_json::Value::Null);
                crate::inference::run_analysis::AnalysisTargets::from_stored(
                    settings.get(crate::inference::run_analysis::ANALYSIS_TARGETS_KEY))?
            }
        };
        start_with_models(
            &app,
            scope.as_deref().unwrap_or("chapter"),
            &chapter_id,
            page_index,
            engine_ceiling,
            picks,
            outside,
            parsed_color,
            mask_padding,
            selection,
            detection,
            selected_models,
            all_text,
            CloudStages { targets, grant: cloud_grant },
            mode,
            area,
        )
    })
    .await
}

/// `cancelRun`. Keeps every completed region.
///
/// Answers only once the worker has stopped and `run-finished` has gone out, so
/// that a caller which cancels and then reads the event stream sees the same
/// thing the mock shows it. `null` when the id names no run in flight. Without
/// an id it cancels the one run in flight, as callers from before runs could
/// overlap expect, and answers `null` when there is none or more than one:
/// guessing which one would stop a run the caller never named.
#[tauri::command]
pub async fn cancel_run(run_id: Option<String>) -> Result<Option<String>, String> {
    crate::library::blocking(move || request_cancel(run_id)).await
}

/// `cancelRun`'s body, off the Tauri boundary so the race it is careful about
/// can be set up in a test.
///
/// **A run that has already finished is not cancellable, and saying it was is a
/// lie the caller can catch.** The window is small and real: `execute` has
/// returned and its `run-finished` has gone out saying `completed`, and the slot
/// is not cleared until the worker's release runs. A cancel arriving in it used
/// to set the flag on a dead run, wait out the release, and answer with the run
/// id - a caller that had just been told the run completed, told a moment later
/// that its cancel took. So the outcome is checked on both sides of the wait,
/// and `null` - "there was nothing to cancel" - is the honest answer.
fn request_cancel(run_id: Option<String>) -> Result<Option<String>, String> {
    let (id, cancel, finished, outcome) = {
        let guard = active().lock().map_err(|e| e.to_string())?;
        let run = match run_id.as_deref() {
            Some(asked) => guard.iter().find(|run| run.run_id == asked),
            None if guard.len() == 1 => guard.first(),
            None => None,
        };
        let Some(run) = run else { return Ok(None) };
        (
            run.run_id.clone(),
            Arc::clone(&run.cancel),
            Arc::clone(&run.finished),
            Arc::clone(&run.outcome),
        )
    };
    if completed_already(&outcome) {
        return Ok(None);
    }
    cancel.store(true, Ordering::SeqCst);

    let (lock, condvar) = &*finished;
    let mut done = lock.lock().map_err(|e| e.to_string())?;
    while !*done {
        let (guard, timeout) = condvar.wait_timeout(done, CANCEL_WAIT).map_err(|e| e.to_string())?;
        done = guard;
        if timeout.timed_out() {
            break;
        }
    }
    drop(done);
    if completed_already(&outcome) {
        return Ok(None);
    }
    Ok(Some(id))
}

/// Whether this run's `run-finished` has already gone out saying it completed.
///
/// A run still going has no outcome yet, and one that stopped because of this
/// cancel - or because it panicked, which reports itself the same way - carries
/// `cancelled`. Only `completed` means the cancel had nothing to act on.
fn completed_already(outcome: &Mutex<Option<&'static str>>) -> bool {
    outcome.lock().map(|reason| *reason == Some("completed")).unwrap_or(false)
}

/// `resumeJob`. Re-verifies the sources, then starts the run.
///
/// "Resume re-verifies source hashes before continuing", and a source that
/// changed loses its patches - the page
/// is stale, and compositing at coordinates taken from a page that has since
/// been re-cropped is the failure the rule exists to prevent. The user is meant
/// to be told, and cannot be: the catalogue has no key for "some pages were
/// dropped because their files changed". Recorded, not invented.
#[tauri::command]
pub async fn resume_job(
    app: tauri::AppHandle,
    project_id: String,
    chapter_id: Option<String>,
) -> Result<Option<ResumedJob>, String> {
    crate::library::blocking(move || {
        let library = Library::for_app(&app).map_err(|e| e.to_string())?;
        let projects = library.list_projects().map_err(|e| e.to_string())?;
        let Some(project) = projects.into_iter().find(|p| p.id == project_id) else {
            return Ok(None);
        };
        let Some(interrupted) = project.interrupted_job.clone() else { return Ok(None) };
        let chapter_id = chapter_id.unwrap_or_else(|| interrupted.chapter_id.clone());
        let Some(chapter) = project.chapters.iter().find(|c| c.id == chapter_id).cloned() else {
            return Ok(None);
        };

        // `ApiProject.interrupted_job` names the *first* interrupted chapter of
        // the project, which is what Home offers. A caller that named a
        // different chapter is resuming that one, and its own manifest is where
        // its resume point is - taking the project's would send the editor to a
        // page of the wrong chapter.
        let mut resumed_from = interrupted.page_index;
        let mut run_snapshot = serde_json::Value::Null;
        {
            let job_path = library.job_path(&project_id, &chapter_id);
            // Read, verify, drop, flush - one read-modify-write, so it is held
            // for the whole of it and released before `start` takes it again.
            let _lock = lock_job(&job_path)?;
            if let Ok(mut job) = Job::open(&job_path) {
                run_snapshot = job.project.settings.clone();
                if let Some(at) = job.project.interrupted_at {
                    resumed_from = at;
                }
                let states = job.verify_sources();
                let _ = job.drop_stale(&states);
            }
        }

        // Resume the captured run policy. Defaults here would silently clean
        // languages skipped before an interruption.
        let policy = captured_run_policy(&run_snapshot)?;
        let mode = policy.mode;
        let handle = start_with_models(
            &app,
            &policy.scope,
            &chapter_id,
            policy.page_index,
            policy.engine_ceiling,
            policy.picks,
            policy.outside,
            policy.bubble_color,
            policy.mask_padding,
            policy.selection,
            policy.detection,
            policy.detector_models,
            policy.all_text,
            // A resume has no consent of its own: a run that sent pages to the
            // cloud is refused here (`cloud_run_grant_required`) and started
            // again from the editor, where the consent is asked.
            CloudStages { targets: policy.analysis_targets, grant: None },
            policy.mode,
            policy.area,
        )?;
        if !resume_started(&handle) { return Ok(None); }
        events::notice(
            "notice.job.resumed",
            serde_json::json!({ "page": resumed_from + 1 }),
            "info",
        );
        Ok(Some(ResumedJob {
            project,
            chapter,
            resumed_from,
            run_id: handle.run_id,
            pages: handle.pages,
            mode: mode.as_str(),
        }))
    })
    .await
}

fn resume_started(handle: &RunHandle) -> bool {
    handle.run_id.is_some() && handle.already_running != Some(true)
}

#[cfg(test)]
mod tests;
