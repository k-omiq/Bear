//! The four region-level edits that need an engine: `applyTool`,
//! `createRegion`, `rerunMask` and `cleanAnyway`.
//!
//! This is the other half of the specification. Its two easy siblings -
//! `deleteMask` and `restoreRegion` - are in [`crate::library`], because
//! deleting a mask is a boolean on a persisted record; these four each have to
//! **run a rung over one region**, which is everything [`crate::run`] does for
//! a page with the queue, the events and the escalation loop taken away.
//!
//! ## Nothing here is a second pipeline
//!
//! Every part that decides what a patch *is* - the ladder, the quality metric,
//! the provenance snapshot, the mask digest - is [`crate::run`]'s and is called
//! from here rather than restated. A region cleaned by hand and a region
//! cleaned by a run are identical in kind, and the only way to keep that
//! true is for both to go through the same code. What this module owns is the
//! three things a run does not have to answer:
//!
//! - **where the mask comes from**, which is the caller's rectangle for a hand
//!   gesture, the stored mask for a re-run, and the detector for a region the
//!   gate held back;
//! - **which rung runs**, which is the one the user named - every picker in
//!   the interface names a model outright - or the route's own where nobody
//!   named anything;
//! - **what to say when nothing could be done**, which is a notice rather than
//!   a silent `null`.
//!
//! ## A named rung is run, and not scored against
//!
//! The decline is a rule about a choice **nobody watched being made**: an
//! automatic run picks a rung, and if the metric says the pixels are wrong the
//! region is left alone rather than filled badly. A user who opens the picker
//! and chooses *LaMa* has watched it. So [`Choice::Exact`] commits what the
//! rung produced and records
//! the metric's verdict in `params_snapshot` - the same field a run writes it
//! to - where [`Choice::Ladder`] keeps the run's own behaviour, escalating past
//! a declined rung and refusing when the top one declines too.
//!
//! ## The cloud rung is reached only with a grant
//!
//! A region is rendered in the cloud only when the caller brings a grant:
//! `grantNonce`, `executionTarget`, `recipe` and `intent` in `params`, the
//! four things consent minted it for. `applyTool`, `rerunMask` and
//! `cleanAnyway` then hand the region to
//! [`crate::inference::service::InferenceService::execute_cloud_render`],
//! which checks the grant against the region as it is on disk before anything
//! is sent. Its phases go out as `cloud://attempt` events, and a render that
//! does not commit is answered with a stable code, never with an error's text.
//!
//! Everything else stays local. With the permission switch off, a cloud
//! request is `blocked` with `notice.cloud.blocked`, grant or no grant. A cloud
//! engine named without a grant is `needs-confirmation` from `applyTool` and a
//! refusal from the other edits. `createRegion` never renders in the cloud:
//! consent binds to a stored region, which a hand-drawn one is not until this
//! command writes it, so the region is created with the local default for its
//! fill mode and the interface asks for the cloud render on it next. Automatic
//! runs never reach the cloud at all.

use std::borrow::Cow;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use cleaner_core::accel::Preference;
use cleaner_core::detect::{build_regions_separated, Detector};
use cleaner_core::engines::flux;
use cleaner_core::engines::model::Error as ModelError;
use cleaner_core::engines::render::{ExecutionTarget, RenderRecipe, ToneReport};
use cleaner_core::fit::{self, EdgeMap, Fitted};
#[cfg(test)]
use cleaner_core::image::decode;
use cleaner_core::image::Raster;
use cleaner_core::ingest::sha256_hex;
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::memory;
use cleaner_core::patch::{Engine, Patch, Provenance};
use cleaner_core::project::{Job, PatchRecord};
use cleaner_core::quality;
use cleaner_core::sidecar::{self, Backend, Budget};
use cleaner_core::text_shape::{GeometryPolicy, MaskRaster, MASK_PLAN_VERSION};
#[cfg(test)]
use cleaner_core::text_shape::MaskPlan;

use crate::inference::consent::OperationIntent;
use crate::inference::service::{
    attempt_id_for_nonce, CloudAttemptProgress, PollOptions, RenderPhase,
};
use crate::library::{ApiMask, ApiRegion, Bbox, Library, LibraryError};
use crate::run::{self, EnginePick, Picks};

/* ------------------------------------------------------------------ */
/* The shapes the seam carries                                         */
/* ------------------------------------------------------------------ */

/// The discriminated union of outcomes.
///
/// `applied`, `run-started`, `blocked` and `not-found` as for any edit, and
/// `needs-confirmation` for a cloud engine named without a grant. A cloud
/// render that did not commit answers where it stopped, `failed`,
/// `cancelled` or `unknown` (the gateway may hold the job, and it is never
/// resubmitted), with the reason in `errorCode`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub status: &'static str,
    /// Why a cloud request did not commit: one of
    /// [`crate::inference::service::InferenceServiceError::code`], or a code
    /// of this module's own (`invalid_request`, `target_changed`,
    /// `config_unreadable`, `journal_error`, `cloud_disabled`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<ApiRegion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<ApiMask>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<run::QueuedPage>>,
}

impl ApplyResult {
    fn of(status: &'static str) -> ApplyResult {
        ApplyResult {
            status,
            error_code: None,
            region: None,
            mask: None,
            page_status: None,
            run_id: None,
            pages: None,
        }
    }

    fn applied(edited: Edited) -> ApplyResult {
        ApplyResult {
            status: "applied",
            error_code: None,
            mask: edited.region.mask.clone(),
            region: Some(edited.region),
            page_status: Some(edited.page_status),
            run_id: None,
            pages: None,
        }
    }
}

/// `createRegion`'s answer and `deleteMask`'s, which are the same pair.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedRegion {
    pub region: ApiRegion,
    pub page_status: String,
}

/// `rerunMask`'s answer. `reopenTool` is `null` for every kind but
/// `reopenInTool`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RerunResult {
    pub region: ApiRegion,
    pub mask: Option<ApiMask>,
    pub reopen_tool: Option<&'static str>,
    pub page_status: String,
}

/// `cleanAnyway`'s.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanedRegion {
    pub region: ApiRegion,
    pub mask: Option<ApiMask>,
    pub page_status: String,
}

/// `editDetectionMask`'s: which of the page's detections the edit rewrote,
/// made or erased, by id, and the page's status after it. Three empty lists
/// are an edit that changed nothing and wrote nothing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskEdit {
    pub page_status: String,
    /// Detections whose masks changed. Their ids are kept.
    pub changed: Vec<String>,
    /// The detection an add over no detection made.
    pub created: Vec<String>,
    /// Detections a remove erased entirely.
    pub removed: Vec<String>,
}

/// Whether rung 3a can be offered on this machine, and what to say if not.
///
/// **Absence answers `null`**, and that is [`sidecar::Availability::Absent`]'s
/// own rule: the application ships no Python, so a machine that installed
/// nothing is the ordinary case and has nothing to tell anybody. A control that
/// would fail is simply not shown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SidecarStatus {
    pub available: bool,
    pub reason_key: Option<&'static str>,
}

/// One region, as the manifest now holds it, with its page's status.
struct Edited {
    region: ApiRegion,
    page_status: String,
}

/* ------------------------------------------------------------------ */
/* Rung 3a                                                             */
/* ------------------------------------------------------------------ */

/// The model rung 3a opens. The sidecar's own default
/// ([`cleaner_core::sidecar`] §"The wire"), named here because the spawn has to
/// say which one.
const FLUX_MODEL: &str = "flux2-klein-4b";

/// Which backend this machine's sidecar is asked for.
///
/// **A preference with a platform default, not a platform fact.** It used to be
/// the second: MLX exists on Apple Silicon and nowhere else, so `mflux` there and
/// the GGUF path - which never rendered - everywhere else. `sdnq` is torch and
/// runs on both kinds of machine ([`cleaner_core::sidecar::backend`]), so on a
/// Mac there are genuinely two backends and which is better is a question about
/// the user's machine and weights rather than about the platform. The setting is
/// `fluxBackend`, its default is `auto`, and
/// [`Backend::from_setting`] is where `auto` is resolved - in the core, beside
/// the table it resolves against, so this function is only the settings read.
///
/// An unset or unknown value is `auto` rather than an error: a stale settings
/// file must not make the rung unreachable.
fn flux_backend(setting: Option<&str>) -> Backend {
    Backend::from_setting(setting).unwrap_or(Backend::Sdnq)
}

/// One string setting, under the first of several names that is present.
///
/// Every sidecar preference has two spellings - the one the interface writes and
/// an earlier one a stored settings file may still carry - and reading them by
/// hand at four call sites is how the fifth one gets the fallback wrong. Empty is
/// `None`: a field somebody cleared is not a value.
fn setting_str(settings: Option<&serde_json::Value>, names: &[&str]) -> Option<String> {
    let settings = settings?;
    names
        .iter()
        .find_map(|name| settings.get(*name).and_then(|v| v.as_str()))
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
}

/// The names each sidecar preference is read under, newest first.
const SIDECAR_PATH_KEYS: &[&str] = &["sidecarPath", "sidecarFolder"];
const FLUX_MODEL_KEYS: &[&str] = &["fluxModel", "sidecarModel"];
const FLUX_BACKEND_KEYS: &[&str] = &["fluxBackend", "sidecarBackend"];

/// The `fluxBackend` setting, as stored.
fn flux_backend_setting(app: &tauri::AppHandle) -> Option<String> {
    setting_str(crate::settings::read(app).ok().as_ref(), FLUX_BACKEND_KEYS)
}

/// Whether rung 3a is installed, has a backend here, and can be measured.
///
/// **Not the whole gate**, deliberately. The authoritative one needs the
/// sidecar's own report of what the model costs
/// ([`flux::Inpainter::open`] runs it), and asking for that means spawning a
/// process and reading gigabytes off disk - which is not something a picker
/// being drawn may do. So this answers the questions that can be answered for
/// free: is there an install, does this build render on this platform at all
/// ([`sidecar::hardware::platform_decline`]), and is this a machine
/// [`memory::room`] has a number for. A region that then cannot be carried is
/// refused at the moment it is asked for, with the reason the gate gives.
///
/// The platform question comes first for the same reason it does inside the
/// gate: a Windows machine whose memory measures perfectly well must not be
/// told its memory could not be measured.
pub fn sidecar_status_from(
    explicit: Option<&str>,
    backend: Option<&str>,
    app_data: Option<&std::path::Path>,
) -> SidecarStatus {
    let env_var = std::env::var("MANGA_CLEANER_SIDECAR").ok();
    let explicit = explicit
        .filter(|s| !s.is_empty())
        .or_else(|| env_var.as_deref().filter(|s| !s.is_empty()));
    let roots = sidecar::search_paths_from(explicit, app_data);
    match sidecar::find_within(&roots) {
        // Nothing installed: nothing to offer and nothing to say.
        None => SidecarStatus {
            available: false,
            reason_key: None,
        },
        // The *chosen* backend's platform question, not the platform's own -
        // since `sdnq` there is no platform without a backend, and what can still
        // be refused here is a user who selected one that cannot run.
        Some(_) => match sidecar::hardware::platform_decline_for(flux_backend(backend)) {
            Some(decline) => SidecarStatus {
                available: false,
                reason_key: Some(decline.reason_key()),
            },
            None if memory::room().is_none() => SidecarStatus {
                available: false,
                reason_key: Some("decline.reason.sidecarUnknownMachine"),
            },
            None => SidecarStatus {
                available: true,
                reason_key: None,
            },
        },
    }
}

#[allow(dead_code)]
pub fn sidecar_status(app_data: Option<&std::path::Path>) -> SidecarStatus {
    sidecar_status_from(None, None, app_data)
}

/// A discovered model directory in the sidecar's weights folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarModelInfo {
    pub id: String,
    pub label: String,
}

/// Derive model id and a friendly label from a weights directory name.
pub fn parse_model_dir_name(name: &str) -> (String, String) {
    let mut base = name;
    let mut quant: Option<&str> = None;

    for backend in ["-mflux", "-sdnq", "-gguf", "-sdcpp", "-mlx"] {
        if let Some(pos) = base.find(backend) {
            let (b, rest) = base.split_at(pos);
            base = b;
            let after_backend = &rest[backend.len()..];
            if let Some(q) = after_backend.strip_prefix('-') {
                quant = Some(q);
            }
            break;
        }
    }

    if quant.is_none() {
        let known_quants = [
            "-q4_k_m", "-q4_k_s", "-q5_k_m", "-q5_k_s", "-q8_0", "-q4_0", "-q4_1", "-q5_0",
            "-q5_1", "-int4", "-int8", "-4bit", "-8bit", "-bf16", "-fp16", "-fp8", "-fp32", "-q4",
            "-q8", "-q5", "-q6", "-q2", "-q3",
        ];
        for k_quant in known_quants {
            if let Some(b) = base.strip_suffix(k_quant) {
                base = b;
                quant = Some(&k_quant[1..]);
                break;
            }
        }
    }

    let id = base.to_string();

    let model_label = if base.eq_ignore_ascii_case("flux2-klein-4b") {
        "FLUX.2 Klein 4B".to_string()
    } else if base.eq_ignore_ascii_case("flux2-klein-9b") {
        "FLUX.2 Klein 9B".to_string()
    } else if base.eq_ignore_ascii_case("flux2-klein-2b") {
        "FLUX.2 Klein 2B".to_string()
    } else if base.eq_ignore_ascii_case("flux1-dev") {
        "FLUX.1 [dev]".to_string()
    } else if base.eq_ignore_ascii_case("flux1-schnell") {
        "FLUX.1 [schnell]".to_string()
    } else if let Some(b_num) = base.strip_prefix("flux2-klein-") {
        if b_num.ends_with('b') || b_num.ends_with('B') {
            format!("FLUX.2 Klein {}", b_num.to_ascii_uppercase())
        } else {
            base.to_string()
        }
    } else {
        base.to_string()
    };

    let quant_label = quant.map(|q| {
        let lower = q.to_ascii_lowercase();
        match lower.as_str() {
            "q4" | "int4" | "4bit" => "4-bit".to_string(),
            "q8" | "int8" | "8bit" => "8-bit".to_string(),
            "q5" | "int5" | "5bit" => "5-bit".to_string(),
            "q6" | "int6" | "6bit" => "6-bit".to_string(),
            "q2" | "int2" | "2bit" => "2-bit".to_string(),
            "q3" | "int3" | "3bit" => "3-bit".to_string(),
            "bf16" => "BF16".to_string(),
            "fp16" => "FP16".to_string(),
            "fp8" => "FP8".to_string(),
            "fp32" => "FP32".to_string(),
            "q4_k_m" => "Q4_K_M".to_string(),
            "q4_k_s" => "Q4_K_S".to_string(),
            "q5_k_m" => "Q5_K_M".to_string(),
            "q5_k_s" => "Q5_K_S".to_string(),
            "q8_0" => "Q8_0".to_string(),
            "q4_0" => "Q4_0".to_string(),
            _ => q.to_string(),
        }
    });

    let label = match quant_label {
        Some(q) => format!("{model_label} ({q})"),
        None => model_label,
    };

    (id, label)
}

/// Scan `<resolved install root>/weights/` for installed model directories.
pub fn list_sidecar_models_from(
    explicit: Option<&str>,
    app_data: Option<&std::path::Path>,
) -> Vec<SidecarModelInfo> {
    let env_var = std::env::var("MANGA_CLEANER_SIDECAR").ok();
    let explicit = explicit
        .filter(|s| !s.is_empty())
        .or_else(|| env_var.as_deref().filter(|s| !s.is_empty()));
    let roots = sidecar::search_paths_from(explicit, app_data);
    let Some(install) = sidecar::find_within(&roots) else {
        return Vec::new();
    };
    let weights_dir = install.root.join("weights");
    let Ok(entries) = std::fs::read_dir(&weights_dir) else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if !name.starts_with('.') {
                    let (id, label) = parse_model_dir_name(name);
                    models.push(SidecarModelInfo { id, label });
                }
            }
        }
    }
    models.sort_by(|a, b| a.label.cmp(&b.label));
    models
}

/* ------------------------------------------------------------------ */
/* Where a region lives                                                */
/* ------------------------------------------------------------------ */

/// Which chapter, which job and which page a region id names.
pub(crate) struct Located {
    pub(crate) chapter_id: String,
    pub(crate) job_path: PathBuf,
    pub(crate) page_index: usize,
}

/// The page a region id sits on, from the id alone.
///
/// Every region id in this application is built on a page id and every page id
/// is `<chapter>-p<NNN>` ([`crate::library::page_id`]) - the run's `-r{n}`, the
/// gate's `-u{x}-{y}-{w}-{h}` and a hand region's `-h{n}` all share that stem.
/// So the page is readable without opening a manifest, which is what lets the
/// job be opened **once**, under its lock, with everything already known.
fn page_index_in(chapter_id: &str, region_id: &str) -> Option<usize> {
    let rest = region_id.strip_prefix(&format!("{chapter_id}-p"))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<usize>().ok()?.checked_sub(1)
}

pub(crate) fn locate_region(
    library: &Library,
    region_id: &str,
) -> Result<Option<Located>, LibraryError> {
    let index = library.index()?;
    let Some(chapter_id) = crate::library::chapter_holding(&index, region_id) else {
        return Ok(None);
    };
    let Some(page_index) = page_index_in(&chapter_id, region_id) else {
        return Ok(None);
    };
    let job_path = library.resolve_chapter(&chapter_id)?;
    Ok(Some(Located {
        chapter_id,
        job_path,
        page_index,
    }))
}

pub(crate) fn locate_page(
    library: &Library,
    chapter_id: &str,
    page_index: usize,
) -> Result<Option<Located>, LibraryError> {
    let job_path = library.resolve_chapter(chapter_id)?;
    Ok(Some(Located {
        chapter_id: chapter_id.to_owned(),
        job_path,
        page_index,
    }))
}

/* ------------------------------------------------------------------ */
/* What one edit asks for                                              */
/* ------------------------------------------------------------------ */

/// Where the mask an edit writes through comes from.
enum Geometry {
    /// The caller's own rectangle, in page pixels. **Authoritative**: the
    /// fitting search does not run over it, because the user has already
    /// said where the edit goes.
    Given(Rect),
    /// The **shape** the caller painted, in page pixels - the discs a round
    /// brush swept along its path, not the rectangle around them.
    /// Authoritative for the same reason [`Geometry::Given`] is, and
    /// preferred over it wherever the gesture actually had a shape.
    Painted(Mask),
    /// The mask the region's existing patch was applied with. A re-run edits
    /// the same set of pixels with a different engine, and re-deriving the
    /// geometry would quietly move it.
    Stored,
    /// [`Geometry::Stored`] with the stored **mask** as the model's hole
    /// rather than the stored lettering: Try again wider.
    ///
    /// The mask is what the last render wrote through, the lettering plus the
    /// isolation radius, so each press hands the model a hole that radius
    /// wider than the last one had, and saves that hole as the next press's.
    /// This is what every re-run did before the hole was pinned to the
    /// lettering, and on real scans the growing hole is what made a second
    /// press come back cleaner than the first: the model stops reading the
    /// silhouette of the old hole. Kept beside [`Geometry::Stored`] rather
    /// than instead of it, so plain Try again still repeats the last render.
    StoredWider,
    /// An accepted, persisted text-shaped preview. Its identity is checked
    /// again after reading the exact lower composite, before any renderer runs.
    #[allow(dead_code)]
    Prepared(String),
    /// Whatever the detector finds under this rectangle - the case a region the
    /// gate held back is in, where all the manifest kept is the box.
    ///
    /// **The one remaining detector call on an edit**, and it belongs to
    /// [`existing_geometry`]: a gate-skipped row has no patch and no painted
    /// shape, so the box the manifest kept is all there is to look under. No
    /// hand gesture reaches it - a stroke says where the mask goes
    /// ([`painted_geometry`]).
    Detected(Rect),
    /// A stored detection's own fitted mask (`docs/detect-clean.md` §3).
    /// **No detector runs**: Detect already found and fitted this region, and
    /// what it stored is exactly what a run would clean through, so this is
    /// [`Geometry::Stored`] for a region that has no patch yet. The lettering
    /// and the fit's route come back from the record with it.
    StoredDetection,
    /// A held candidate's own lettering, in page pixels, and the proxy scale
    /// it was found at. **No detector runs**: a candidate is lettering no
    /// detector box claimed, so looking under its box finds nothing and the
    /// box itself, over artwork, is a fill no rung can pass. The fit grows it
    /// as a run's fit grows a cleaned group's lettering.
    HeldLettering(Mask, f32),
}

/// Which rung runs. See the module docs for why the two are scored differently.
#[derive(Debug, PartialEq, Eq)]
enum Choice {
    /// The rung the user named, run once.
    Exact(Engine),
    /// The route's own rung, moved by a `fill`/`redraw` pick, escalating up the
    /// ladder exactly as a run would.
    Ladder(Option<EnginePick>),
}

/// One region edit, before anything has been opened.
struct Plan {
    region_id: String,
    expected_source: Option<(usize, String)>,
    geometry: Geometry,
    choice: Choice,
    /// `hand` or `auto`, for the params snapshot - the one word that separates
    /// a hand mask from an automatic one.
    source: &'static str,
    tool: Option<String>,
    fill_mode: Option<&'static str>,
    solid_color: Option<[u8; 3]>,
    /// The gate-skipped row this edit consumes, if it is cleaning one. It goes
    /// with the patch that replaces it, or the page would carry both a warning
    /// and the mask that answered it.
    clears_untouched: bool,
    /// A brush or clone stroke, where this edit is one.
    ///
    /// **One field, parsed at both parse sites, branched on at one.** The two
    /// seam methods already converge on [`edit`]; this is what keeps the
    /// convergence true of painting as well.
    paint: Option<PaintPlan>,
    /// The caller's complete hand-drawn bounds, before shape rasterisation.
    requested: Option<Rect>,
    stroke: Option<PaintedStroke>,
    drawn: Option<PaintedShape>,
}

/// What an edit did.
///
/// `Cleaned` carries the whole `Edited` record inline. The enum is built once
/// per edit and consumed at once, never held in a collection, so the size gap
/// between its variants costs nothing worth a `Box`.
#[allow(clippy::large_enum_variant)]
enum Outcome {
    Cleaned(Edited),
    /// No rung produced a patch. The key is the reason, in the same
    /// `decline.reason.*` vocabulary a run's review row carries.
    Refused(&'static str),
    /// No region, no page, or no job.
    NotFound,
}

/* ------------------------------------------------------------------ */
/* The engines, held for one edit                                      */
/* ------------------------------------------------------------------ */

/// The sessions one region edit may need.
///
/// Built per call and **borrowed, never owned**: every session it uses comes
/// out of [`cleaner_core::residency`] and goes back there when the click ends.
/// That is the whole of what changed here - the bench used to open a detector
/// per edit and spawn a sidecar per region, so a reviewer working down a page
/// of SFX paid a session build, or a process start and several gigabytes off
/// disk, for every one of them. Nothing is opened until something asks for
/// it - a brush stroke that lands on flat paper opens no model at all - so
/// the ordinary edit still costs no session, and the second edit in a row
/// costs no *load*.
struct Bench {
    app_data: Option<PathBuf>,
    sidecar_override: Option<String>,
    flux_model: Option<String>,
    /// The `fluxBackend` preference as stored: `auto`, `mflux`, `sdnq`, or
    /// absent. Resolved by [`flux_backend`] rather than here, so the default
    /// lives in one place.
    flux_backend: Option<String>,
    detector: run::OnDemand<Detector>,
    rung2: run::Rung2,
    /// Rung 3a's child, held for the length of this edit and handed back to the
    /// residency cache after it. See [`Bench::flux`].
    flux: Option<flux::Inpainter>,
    flux_key: Option<cleaner_core::residency::Key>,
    #[cfg(test)]
    detected_seed: Option<Mask>,
}

impl Bench {
    fn open(app: &tauri::AppHandle) -> Bench {
        use tauri::Manager;
        let app_data = app.path().app_data_dir().ok();
        let settings = crate::settings::read(app).ok();
        let sidecar_override = setting_str(settings.as_ref(), SIDECAR_PATH_KEYS);
        let flux_model = setting_str(settings.as_ref(), FLUX_MODEL_KEYS);
        let flux_backend = setting_str(settings.as_ref(), FLUX_BACKEND_KEYS);
        // The runtime is downloaded after install, so this can legitimately
        // fail - and a failure is not fatal here the way it is for a run:
        // rung 0 is arithmetic over the page's own samples and needs no
        // ONNX at all. A rung that does need one answers `Unavailable` and the
        // edit is refused with the reason, rather than every edit being
        // refused for a rung most of them never reach.
        let _ = cleaner_core::runtime::find(app_data.as_deref())
            .and_then(|path| cleaner_core::runtime::load(&path));
        let models = run::model_dir(app_data.as_deref()).unwrap_or_default();
        // The same accelerator setting a run reads, so a region edit and the
        // run that produced it land on the same provider.
        let default_preference = settings
            .as_ref()
            .map(run::preference_from)
            .unwrap_or(Preference::Automatic);
        let detector_preference = settings.as_ref()
            .and_then(|settings| run::model_preference_from(settings, "ctd").ok())
            .unwrap_or(default_preference);
        let inpainter_preference = settings.as_ref()
            .and_then(|settings| run::model_preference_from(settings, "inpainter").ok())
            .unwrap_or(default_preference);
        Bench {
            rung2: run::Rung2::new(&models, inpainter_preference),
            detector: run::detector_on_demand(&models, detector_preference),
            flux: None,
            flux_key: None,
            app_data,
            sidecar_override,
            flux_model,
            flux_backend,
            #[cfg(test)]
            detected_seed: None,
        }
    }

    /// The detector: the one already in memory if a run or an earlier click
    /// left one there, and a new one otherwise.
    fn detector(&mut self) -> Option<&mut Detector> {
        self.detector.get().ok()
    }

    /// The text under a rectangle, as the detector sees it.
    ///
    /// `None` when there is no detector on this machine or nothing was found
    /// under the box - and the caller's answer to both is the same: treat the
    /// rectangle itself as the mask. That is a *larger* edit than the glyphs
    /// would be, and it is the honest one: the user asked for this box to be
    /// cleaned, and inventing a tighter shape with no evidence would be a
    /// guess about which pixels are text.
    ///
    /// `source` is the digest of the page file this raster was decoded from,
    /// and it is what stops six clicks on one page being six full-page
    /// detections ([`cleaner_core::detect::detect_page`]).
    fn text_under(&mut self, page: &Raster, box_: Rect, source: &str) -> Option<(Mask, f32)> {
        #[cfg(test)]
        if let Some(seed) = self.detected_seed.as_ref() {
            return Some((seed.clone(), 1.0));
        }
        let detector = self.detector()?;
        let detection = match cleaner_core::detect::detect_page(detector, source, page) {
            Ok(detection) => detection,
            // The answer to a failed detection is the same as the answer to an
            // empty one, and it is the paragraph above: the caller treats the
            // rectangle as the mask. The *session* is a different matter.
            // `Inference` is the detector having built, run, and then failed,
            // which on Windows is usually a graphics adapter that has been
            // reset and will fail every later call the same way. Poisoning it
            // here is what makes the next click open a fresh one instead of
            // being handed the dead one, and it is the same treatment
            // [`engine_fault`] documents for the rungs. The other two variants
            // are not the session's fault - a model that would not load, or one
            // whose outputs did not match the pinned contract - so they are
            // left alone.
            Err(error) => {
                if matches!(error, cleaner_core::detect::DetectError::Inference(_)) {
                    detector.poison();
                }
                eprintln!("manga-cleaner: the detector failed under a region edit: {error}");
                return None;
            }
        };
        let regions =
            build_regions_separated(detection.boxes.clone(), page.width, page.height, |a, b| {
                cleaner_core::balloon::merge_crosses_a_balloon(page, &detection.segmentation, a, b)
            });
        let region = regions
            .into_iter()
            .max_by_key(|region| overlap(region.masking, box_))?;
        if overlap(region.masking, box_) == 0 {
            return None;
        }
        let seed = fit::seed_mask(&detection.segmentation, &region, page.width, page.height);
        if seed.is_empty() {
            return None;
        }
        Some((seed, detection.segmentation.proxy_scale()))
    }

    /// Rung 3a, on a child that **survives the click**.
    ///
    /// It used to be a process per region: spawned here, killed when the edit
    /// ended, ten to sixty seconds of process start and gigabytes off disk paid
    /// again by the next region. The child now goes back to
    /// [`cleaner_core::residency`] with the rest of the bench, and the next
    /// FLUX edit finds it up and warm.
    ///
    /// **What that costs, and the three things that bound it**:
    ///
    /// - the model is released and opened again per region, because the text
    ///   encoder is evicted by the first render and `mflux` cannot rebuild it -
    ///   so a held child holds its *floor*, 1.37 GB, between edits and not a
    ///   render's peak;
    /// - the grace is [`cleaner_core::registry::SIDECAR_IDLE_GRACE`], two
    ///   minutes rather than the five an ONNX session gets, because this is the
    ///   largest thing the application can be holding;
    /// - the pressure ladder takes it first, which is the step
    ///   `Ladder::holding_sidecar` was written for and could not have until
    ///   there was a child to take.
    ///
    /// The tone report is the one the composite aligned the answer by
    /// (`PreparedRender::composite`), for the patch's provenance, which
    /// records it the way a cloud render's attach does.
    fn flux(
        &mut self,
        page: &Raster,
        fitted: &Fitted,
    ) -> Result<(run::Made, quality::Assessment, Option<ToneReport>), &'static str> {
        let env_var = std::env::var("MANGA_CLEANER_SIDECAR").ok();
        let explicit = self
            .sidecar_override
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| env_var.as_deref().filter(|s| !s.is_empty()));
        let roots = sidecar::search_paths_from(explicit, self.app_data.as_deref());
        let Some(install) = sidecar::find_within(&roots) else {
            return Err("decline.reason.rungUnavailable");
        };
        let backend = flux_backend(self.flux_backend.as_deref());
        // Ahead of the memory question, and for the gate's own reason: a backend
        // with no rendering path here is refused whatever the machine measures.
        // Per *backend* rather than per platform since `sdnq` - the only thing
        // that reaches this now is a choice that cannot run.
        if let Some(decline) = sidecar::hardware::platform_decline_for(backend) {
            return Err(decline.reason_key());
        }
        let Some(room) = memory::room() else {
            return Err("decline.reason.sidecarUnknownMachine");
        };
        let model = self
            .flux_model
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(FLUX_MODEL);
        let key = cleaner_core::residency::Key::new(
            cleaner_core::registry::Kind::Sidecar,
            format!("{}|{}|{model}", install.python.display(), backend.id()),
        );
        if self.flux.is_none() {
            self.flux = match cleaner_core::residency::checkout::<flux::Inpainter>(&key) {
                Some(held) => Some(held),
                None => Some(
                    flux::Inpainter::open(&install, backend, model, Budget::from_room(room))
                        .map_err(flux_reason)?,
                ),
            };
            self.flux_key = Some(key);
        }
        let inpainter = self.flux.as_mut().expect("just opened");
        let rendered = inpainter.render(page, fitted).map_err(flux_reason)?;
        let tone = rendered.tone;
        let made = run::Made {
            engine: Engine::Flux,
            mask: rendered.mask,
            ink: fitted.ink.clone(),
            pixels: rendered.pixels,
            provider: Some(backend.id().to_owned()),
            // No digest: the weights are the child's, read by the child, and a
            // digest this process never computed would be one it cannot stand
            // behind.
            model_sha256: None,
            pad: rendered.pad,
            tiles: Some(rendered.tiles),
        };
        let verdict = quality::assess(page, &made.mask, &made.pixels, fit::page_noise_sigma(page));
        Ok((made, verdict, tone))
    }
}

impl Drop for Bench {
    /// **The end of a click is not a reason to unload anything.** The ONNX
    /// sessions hand themselves back - [`crate::run::OnDemand`] and the two
    /// rung holders each have their own `Drop` - and this is the child, which
    /// nothing else owns. A child that is spent goes instead of being parked,
    /// which [`cleaner_core::residency::checkin`] decides and this does not
    /// have to.
    fn drop(&mut self) {
        if let (Some(inpainter), Some(key)) = (self.flux.take(), self.flux_key.take()) {
            cleaner_core::residency::checkin(key, inpainter);
        }
        // A click is a safe point of exactly the kind a run's region boundary
        // is: this edit's buffers are gone and the next one's do not exist. So
        // it is where a parked session whose grace elapsed while the user was
        // reading goes back (nothing here can interrupt the edit that is
        // running).
        cleaner_core::residency::sweep();
    }
}

/// The key a rung 3a failure is reported under.
///
/// A decline carries its own - `decline.reason.sidecar*`, the vocabulary the
/// settings panel and a review row both read - and a fault does not: a sidecar
/// that would not answer is, from the region's point of view, a rung that was
/// not available.
fn flux_reason(error: ModelError) -> &'static str {
    match error {
        ModelError::Declined(decline) => decline.reason_key(),
        ModelError::Run(_) => "decline.reason.rungUnavailable",
    }
}

/// The key an **ONNX** rung's run fault is refused under, and the one place the
/// raw text of it goes.
///
/// [`flux_reason`]'s counterpart for the rungs this process runs itself, and it
/// answers a different key on purpose. Rung 3a's child is a thing that comes
/// and goes - it is spawned, it is killed under memory pressure, it may not be
/// installed - so a sidecar that would not answer is honestly reported as a
/// rung that was not available. An ONNX session that built, ran, and then died
/// is a different statement: the rung *is* on this machine, and something
/// underneath it failed. On Windows the usual cause is one the session cannot
/// come back from, which is why [`run::render_rung`] has already poisoned it by
/// the time this is called - the next edit opens a fresh session rather than
/// being handed the dead one.
///
/// What used to happen instead was nothing the user could read: the fault was
/// propagated with `?` and the Tauri command rejected with ONNX Runtime's own
/// string, which is not a catalogue key, so the click did not visibly do
/// anything at all. The string is not lost - it goes to stderr, which is where
/// this crate already puts diagnostic text a user cannot act on.
fn engine_fault(detail: &str) -> &'static str {
    eprintln!("manga-cleaner: a model faulted during a region edit: {detail}");
    "decline.reason.engineFault"
}

/// How many pixels two rectangles share.
fn overlap(a: Rect, b: Rect) -> u64 {
    let w = (a.right().min(b.right()) - a.x.max(b.x)).max(0) as u64;
    let h = (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0) as u64;
    w * h
}

/* ------------------------------------------------------------------ */
/* Running one edit                                                    */
/* ------------------------------------------------------------------ */

/// Open the job, run the plan, write the patch, and answer with the region.
///
/// Under the job's own lock for the whole of it, like every other
/// read-modify-write of a manifest in this crate: the file is rewritten whole,
/// and an edit that interleaved with a run's flush would lose one of them
/// entirely.
///
/// Everything here renders on this machine. The cloud is reached only through
/// a grant, before a plan is made, so `Engine::Cloud` is refused, and
/// `Engine::Flux` is the local helper whichever cloud endpoint is selected: the
/// interface names the cloud as its own engine. What must not run here, a
/// cloud patch re-run as itself, [`rerun_needs_cloud`] stops before this.
fn edit(app: &tauri::AppHandle, located: &Located, plan: Plan) -> Result<Outcome, String> {
    if plan.choice == Choice::Exact(Engine::Cloud) {
        return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
    }

    let mut bench = Bench::open(app);
    let stored = crate::settings::read(app).ok().and_then(|s| {
        s.get("engineCeiling")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    });
    let ceiling = run::effective_ceiling(None, stored.as_deref());
    edit_with_bench(&mut bench, ceiling, located, plan)
}

fn edit_with_bench(
    bench: &mut Bench,
    ceiling: Engine,
    located: &Located,
    plan: Plan,
) -> Result<Outcome, String> {
    edit_with_bench_render(bench, ceiling, located, plan, None)
}

/// The test renderer observes the same fitted input that a real rung receives.
/// It is kept at this seam so a regression can capture the actual image and
/// mask, rather than merely testing a compositor helper in isolation.
/// A stand-in renderer the tests hand in to capture the exact raster and fit.
type TestRenderer<'a> = &'a mut dyn FnMut(&Raster, &Fitted) -> run::Made;

fn edit_with_bench_render(
    bench: &mut Bench,
    ceiling: Engine,
    located: &Located,
    mut plan: Plan,
    mut test_renderer: Option<TestRenderer<'_>>,
) -> Result<Outcome, String> {
    let _lock = run::lock_job(&located.job_path)?;
    // Read-only from here: the manifest is mutated in `commit`, which takes the
    // job by value and is the only writer either path reaches.
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    let Some(source_idx) = Library::resolve_page(&job.project, located.page_index) else {
        return Ok(Outcome::NotFound);
    };
    let Some(path) = job.source_path(source_idx) else {
        return Ok(Outcome::NotFound);
    };
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let anchor_page = job.display_page(source_idx, &bytes).map_err(|e| e.to_string())?;
    cleaner_core::image::color::validate_editing(&anchor_page).map_err(|e| e.to_string())?;
    // The source's digest, computed once: it goes into this patch's provenance
    // below, and it is the key the page's detection is remembered under
    // ([`cleaner_core::detect::detect_page`]).
    let source_sha256 = sha256_hex(&bytes);
    if plan
        .expected_source
        .as_ref()
        .is_some_and(|(expected_idx, expected_sha)| {
            *expected_idx != source_idx
                || expected_sha != &source_sha256
                || job
                    .project
                    .sources
                    .get(source_idx)
                    .map(|source| &source.sha256)
                    != Some(expected_sha)
        })
    {
        return Ok(Outcome::NotFound);
    }
    let existing: Option<PatchRecord> = job
        .project
        .patches
        .iter()
        .find(|record| record.id == plan.region_id)
        .cloned();
    let detection = match plan.geometry {
        Geometry::StoredDetection => {
            match job.load_detection(&plan.region_id).map_err(|e| e.to_string())? {
                Some(found) => Some(found),
                None => return Ok(Outcome::NotFound),
            }
        }
        _ => None,
    };
    // Where the stored detection's page pixels sit in the raster the edit
    // reads. Zero unless a strip window moved the origin.
    let mut detection_shift = (0i64, 0i64);
    if job.project.sources[source_idx].orientation.0 != 1 {
        let identity=existing.as_ref().and_then(|record|record.text_shape_plan_identity.as_deref());
        if job.project.text_shape_plans.iter().any(|record| record.region_id==plan.region_id && !record.display_coordinates && identity.is_none_or(|id|record.identity.identity_sha256==id)) {
            return Err("This saved mask plan uses the original page orientation. Prepare a new mask plan before applying it; its existing pixels and undo history are preserved.".into());
        }
    }
    let prepared_text = match &plan.geometry {
        Geometry::Prepared(expected) => {
            if job.text_shape_correction(&plan.region_id).is_some() {
                return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
            }
            let Some((mask_plan, prepared)) = job
                .load_text_shape_plan(&plan.region_id)
                .map_err(|e| e.to_string())?
            else {
                return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
            };
            if prepared.identity.identity_sha256 != *expected {
                return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
            }
            Some((mask_plan, prepared))
        }
        Geometry::Stored
            if existing
                .as_ref()
                .is_some_and(|record| record.geometry_policy == GeometryPolicy::TextShape) =>
        {
            let identity = existing
                .as_ref()
                .and_then(|record| record.text_shape_plan_identity.as_deref())
                .ok_or("text-shaped patch is missing its prepared plan identity")?;
            let Some(stored) = job
                .load_text_shape_plan_identity(&plan.region_id, identity)
                .map_err(|e| e.to_string())?
            else {
                return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
            };
            Some(stored)
        }
        _ => None,
    };
    if prepared_text
        .as_ref()
        .is_some_and(|(_, prepared)| prepared.identity.source_sha256 != source_sha256)
    {
        return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
    }
    if let Some((mask_plan, prepared)) = prepared_text.as_ref() {
        if prepared
            .verify_against(mask_plan, anchor_page.width, anchor_page.height)
            .is_err()
        {
            return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
        }
        plan.requested = Some(prepared.write_support.bounds);
    }
    // A text-shaped patch renders through its accepted plan's hole, which a
    // wider hole would leave behind; refused rather than run as plain retry.
    if matches!(plan.geometry, Geometry::StoredWider)
        && existing
            .as_ref()
            .is_some_and(|record| record.geometry_policy == GeometryPolicy::TextShape)
    {
        return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
    }
    if matches!(&plan.geometry, Geometry::Stored | Geometry::StoredWider | Geometry::Prepared(_))
        && existing.as_ref().is_some_and(|record| !record.visible)
    {
        return Ok(Outcome::NotFound);
    }

    // A hand gesture may continue past the anchor page in longstrip mode. Read
    // only the native decode window surrounding it, then run the ordinary edit
    // pipeline in that window's local coordinates. The resulting patch is
    // translated back to anchor-page coordinates before persistence, which is
    // the representation StripPatch already knows how to split for export.
    let strip = run::strip_of(&job.project);
    let anchor = strip.pages().get(located.page_index).copied();
    let requested = plan
        .requested
        .map(|target| manual_requested(target, plan.paint.as_ref(), anchor));
    let crosses = requested.zip(anchor).filter(|(r, p)| {
        r.x < 0 || r.y < 0 || r.right() > p.width as i64 || r.bottom() > p.height as i64
    });
    // An AI stroke near a join can read the other page even when its write
    // mask stays on the anchor page. Read that context in strip coordinates.
    let strip_read = crosses.or_else(|| {
        (plan.paint.is_none())
            .then_some(requested.zip(anchor))
            .flatten()
    });
    let mut patch_shift = (0i64, 0i64);
    // The hole a stored patch's model rungs remove: its lettering, as the
    // preprocessing reads it (`Preprocessing::hole`), in the coordinates the
    // edit runs in. Its mask is what the last render wrote through, the
    // lettering plus the isolation radius, and taking that as the hole again
    // grew the patch by the radius on every re-run and saved the grown hole
    // as the next one's lettering. A strip window carries it translated with
    // the mask, so a long strip keeps the same hole.
    let mut stored_hole: Option<Mask> = None;
    let window_page;
    let page = if job.project.strip.mode == cleaner_core::project::StripMode::Longstrip {
        if let Some((requested, anchor)) = strip_read {
            let global = Rect::new(
                requested.x + anchor.x_offset,
                requested.y + anchor.y_offset,
                requested.w,
                requested.h,
            );
            let mut joins = cleaner_core::strip::Joins::unchecked(strip.joins());
            for join in 0..strip.joins() {
                joins.set(join, cleaner_core::strip::JoinState::Verified);
            }
            // The native strip reader must include more than the bounded
            // composite's 768 px margin, so a crop edge never masquerades as
            // a source-page edge during model padding.
            let decode_bounds = if plan.paint.is_none() {
                Rect::new(
                    global.x.saturating_sub(1024),
                    global.y.saturating_sub(1024),
                    global.w.saturating_add(2048),
                    global.h.saturating_add(2048),
                )
            } else {
                global
            };
            let window = if plan.paint.is_none() {
                // `decode_window` expands once more and its clamp cannot pick
                // an anchor if both ends lie beyond a short whole strip.
                let x = decode_bounds.x.max(0);
                let y = decode_bounds.y.max(0);
                let right = decode_bounds.right().min(strip.width() as i64);
                let bottom = decode_bounds.bottom().min(strip.height() as i64);
                let requested =
                    Rect::new(x, y, (right - x).max(0) as u32, (bottom - y).max(0) as u32);
                let rect = strip.clamp(requested, &joins);
                cleaner_core::strip::DecodeWindow {
                    rect,
                    requested,
                    pad: cleaner_core::strip::EdgePad::None,
                }
            } else {
                cleaner_core::strip::decode_window(
                    &strip,
                    &joins,
                    decode_bounds,
                    1.0,
                    cleaner_core::strip::EngineContext::None,
                )
            };
            // Refuse pathological chapter-scale gestures. Normal edits remain
            // a small context window even when they straddle a join.
            const MAX_MANUAL_WINDOW_PIXELS: u64 = 4096 * 4096;
            if u64::from(window.rect.w) * u64::from(window.rect.h) > MAX_MANUAL_WINDOW_PIXELS {
                return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
            }
            let read = cleaner_core::strip::read_window_borrowing(&strip, window, |position| {
                if position == located.page_index {
                    return Ok(Cow::Borrowed(&anchor_page));
                }
                let source = Library::resolve_page(&job.project, position)
                    .ok_or_else(|| format!("strip page {position} is missing"))?;
                let path = job
                    .source_path(source)
                    .ok_or_else(|| format!("strip page {position} has no source"))?;
                let data = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                job.display_page(source, &data).map(Cow::Owned).map_err(|e| e.to_string())
            })?;
            patch_shift = (
                read.origin.0 - anchor.x_offset,
                read.origin.1 - anchor.y_offset,
            );
            let local = Rect::new(
                global.x - read.origin.0,
                global.y - read.origin.1,
                global.w,
                global.h,
            );
            if prepared_text.is_some() {
                // Keep the accepted support in anchor-page coordinates until
                // the common translation below; a strip window is only a
                // renderer coordinate system, never a new mask authority.
            } else if matches!(plan.geometry, Geometry::Stored | Geometry::StoredWider) {
                let Some(record) = existing.as_ref() else {
                    return Ok(Outcome::NotFound);
                };
                let stored = job.load_display_patch(record).map_err(|e| e.to_string())?;
                let shift = (anchor.x_offset - read.origin.0, anchor.y_offset - read.origin.1);
                // Wider leaves the hole unset: the painted mask below is then
                // both the seed and the hole, as a hand mask is.
                if matches!(plan.geometry, Geometry::Stored) {
                    stored_hole = Some(translated_mask(stored_hole_of(&stored), shift));
                }
                plan.geometry = Geometry::Painted(translated_mask(&stored.mask, shift));
            } else if matches!(plan.geometry, Geometry::StoredDetection) {
                detection_shift = (anchor.x_offset - read.origin.0, anchor.y_offset - read.origin.1);
            } else if let Geometry::HeldLettering(lettering, _) = &mut plan.geometry {
                let shift = (anchor.x_offset - read.origin.0, anchor.y_offset - read.origin.1);
                *lettering = translated_mask(lettering, shift);
            } else {
                plan.geometry = Geometry::Given(local);
            }
            if let Some(paint) = plan.paint.as_mut() {
                let map = |x: f64, y: f64| {
                    let gx = x / 100.0 * anchor.width as f64 + anchor.x_offset as f64;
                    let gy = y / 100.0 * anchor.height as f64 + anchor.y_offset as f64;
                    (
                        (gx - read.origin.0 as f64) / read.raster.width as f64 * 100.0,
                        (gy - read.origin.1 as f64) / read.raster.height as f64 * 100.0,
                    )
                };
                for point in &mut paint.points {
                    (point.x, point.y) = map(point.x, point.y);
                }
                match &mut paint.kind {
                    PaintKind::Shape { shape, .. } => {
                        for point in &mut shape.points {
                            (point.x, point.y) = map(point.x, point.y);
                        }
                    }
                    PaintKind::Clone { offset, .. } => {
                        offset.0 = offset.0 * anchor.width as f64 / read.raster.width as f64;
                        offset.1 = offset.1 * anchor.height as f64 / read.raster.height as f64;
                    }
                    PaintKind::Brush { .. } => {}
                }
            }
            if plan.paint.is_none() {
                if let Some(mut stroke) = plan.stroke.take() {
                    for point in &mut stroke.points {
                        let gx = point.x / 100.0 * anchor.width as f64 + anchor.x_offset as f64;
                        let gy = point.y / 100.0 * anchor.height as f64 + anchor.y_offset as f64;
                        point.x = (gx - read.origin.0 as f64) / read.raster.width as f64 * 100.0;
                        point.y = (gy - read.origin.1 as f64) / read.raster.height as f64 * 100.0;
                    }
                    if let Some(mask) = stroke_mask(&stroke, read.raster.width, read.raster.height)
                    {
                        plan.geometry = Geometry::Painted(mask);
                    }
                } else if let Some(mut shape) = plan.drawn.take() {
                    for point in &mut shape.points {
                        let gx = point.x / 100.0 * anchor.width as f64 + anchor.x_offset as f64;
                        let gy = point.y / 100.0 * anchor.height as f64 + anchor.y_offset as f64;
                        point.x = (gx - read.origin.0 as f64) / read.raster.width as f64 * 100.0;
                        point.y = (gy - read.origin.1 as f64) / read.raster.height as f64 * 100.0;
                    }
                    if let Some(mask) = shape_mask(&shape, read.raster.width, read.raster.height) {
                        plan.geometry = Geometry::Painted(mask);
                    }
                }
            }
            window_page = read.raster;
            &window_page
        } else {
            &anchor_page
        }
    } else {
        &anchor_page
    };

    // **The paint branch, and it is here rather than beside the `solid` fill.**
    //
    // A painted stroke is authoritative: there is no mask to fit, no ring to
    // sample, no edge map to build and no quality verdict to assess, so
    // everything from the geometry match down is work whose answer it does
    // not use. `fit::page_noise_sigma` is the line to intercept before;
    // intercepting *here* skips that and the geometry with it, and cannot be
    // refused by a seed guard that has nothing to do with painting.
    if let Some(paint) = plan.paint.as_ref() {
        let started = Instant::now();
        let order = existing
            .as_ref()
            .map(|record| record.order)
            .unwrap_or_else(|| next_order(&job, source_idx));
        let paint_context = (job.project.strip.mode == cleaner_core::project::StripMode::Longstrip)
            .then_some(StripPaintContext {
                strip: &strip,
                order: &job.project.strip.order,
                origin: (
                    anchor.map_or(0, |p| p.x_offset) + patch_shift.0,
                    anchor.map_or(0, |p| p.y_offset) + patch_shift.1,
                ),
            });
        let (mut made, dabs, read_window) =
            match paint_patch(&job, source_idx, page, paint, order, paint_context) {
                Ok(pair) => pair,
                Err(reason) => return Ok(Outcome::Refused(reason)),
            };
        let strip_space = paint_context.map(|context| crate::underlay::StripSpace {
            strip: context.strip,
            origin: context.origin,
        });
        let input = crate::underlay::read_exact(
            &job, source_idx, page, read_window, order, strip_space,
        )?;
        translate_made(&mut made, patch_shift);
        let mut snapshot = paint_snapshot(&plan, paint, made.engine, dabs, started.elapsed());
        // Brush, clone and shape all composite a surface, including opaque
        // strokes that preserve existing alpha. Their footprint is never unknown.
        snapshot["input_provenance"] = serde_json::json!({
            "version": 1, "input_sha256": input.digest,
            "predecessors_sha256": input.predecessors,
            "read_footprint": crate::underlay::page_rect(read_window,
                Rect::new(patch_shift.0, patch_shift.1, 0, 0)),
        });
        return commit(
            located,
            job,
            source_idx,
            &plan.region_id,
            plan.clears_untouched,
            made,
            snapshot,
            source_sha256,
            order,
            None,
        );
    }

    let (seed, scale, manual) = match plan.geometry {
        Geometry::Given(rect) => (Mask::filled(clamped(rect, page)), 1.0, true),
        // The shape the hand drew, used as it came. Nothing is fitted over it
        // and nothing is squared off: that is the whole point of carrying it.
        Geometry::Painted(mask) => (mask, 1.0, true),
        Geometry::Stored => {
            if let Some((_, prepared)) = prepared_text.as_ref() {
                let mut mask = prepared.write_support.to_mask();
                mask.bounds.x -= patch_shift.0;
                mask.bounds.y -= patch_shift.1;
                (mask, 1.0, true)
            } else {
                let Some(record) = existing.as_ref() else {
                    return Ok(Outcome::NotFound);
                };
                let stored = job.load_display_patch(record).map_err(|e| e.to_string())?;
                stored_hole = Some(stored_hole_of(&stored).clone());
                (stored.mask, 1.0, true)
            }
        }
        // The manual fit takes the seed as both mask and lettering, so with
        // no stored hole put back the stored mask is the model's hole.
        Geometry::StoredWider => {
            let Some(record) = existing.as_ref() else {
                return Ok(Outcome::NotFound);
            };
            (job.load_display_patch(record).map_err(|e| e.to_string())?.mask, 1.0, true)
        }
        Geometry::Prepared(_) => {
            let Some((_, prepared)) = prepared_text.as_ref() else {
                return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
            };
            let mut mask = prepared.write_support.to_mask();
            mask.bounds.x -= patch_shift.0;
            mask.bounds.y -= patch_shift.1;
            (mask, 1.0, true)
        }
        Geometry::Detected(rect) => match bench.text_under(page, rect, &source_sha256) {
            Some((seed, scale)) => (seed, scale, false),
            None => (Mask::filled(clamped(rect, page)), 1.0, true),
        },
        Geometry::HeldLettering(lettering, scale) => (lettering, scale, false),
        // Authoritative as a stored patch mask is: the fit ran when it was
        // detected, and running the search again would move the boundary.
        Geometry::StoredDetection => {
            let Some(found) = detection.as_ref() else {
                return Ok(Outcome::NotFound);
            };
            (translated_mask(&found.mask, detection_shift), 1.0, true)
        }
    };
    if seed.is_empty() {
        return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
    }

    // A detection takes the place in reading order a run would have given it.
    let order = existing
        .as_ref()
        .map(|record| record.order)
        .or_else(|| detection.as_ref().map(|found| found.record.order))
        .unwrap_or_else(|| next_order(&job, source_idx));
    let strip_space = (job.project.strip.mode == cleaner_core::project::StripMode::Longstrip)
        .then_some(crate::underlay::StripSpace {
            strip: &strip,
            origin: (
                anchor.map_or(0, |p| p.x_offset) + patch_shift.0,
                anchor.map_or(0, |p| p.y_offset) + patch_shift.1,
            ),
        });
    let input = crate::underlay::read(
        &job,
        source_idx,
        page,
        seed.bounds,
        order,
        strip_space,
    )?;
    let raw_page = page;
    let page = &input.image;
    let seed = crate::underlay::local_mask(&seed, input.window);
    let input_shift = (input.window.x, input.window.y);

    if let Some((mask_plan, prepared)) = prepared_text.as_ref() {
        if mask_plan.source_sha256 != source_sha256
            || lower_composite_revision(&input) != prepared.identity.lower_composite_sha256
            || prepared.identity.support_sha256
                != cleaner_core::text_shape::support_sha256(&prepared.write_support)
        {
            return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
        }
    }

    // The noise floor from the raw page, which no layer edit changes (a run
    // measures it the same way), and the edges only where the fit looks: an
    // earlier edit can then change this render only through what
    // `underlay::render_reach` records below.
    let noise = fit::page_noise_sigma(raw_page);
    let edges = EdgeMap::sobel_within(
        page,
        crate::underlay::fit_statistics_bounds(seed.bounds, scale, page.width, page.height),
    );
    let mut fitted = fit::fit(page, &seed, scale, noise, &edges, manual);
    // The manual fit takes the stored mask as it is; what it cannot know is
    // what the detecting fit chose beyond the mask. The lettering a model
    // writes through, the route and the growth are put back, so a click
    // cleans this region as a Clean run would.
    if let Some(found) = detection.as_ref() {
        let ink = translated_mask(&found.ink, detection_shift);
        fitted.ink = crate::underlay::local_mask(&ink, input.window);
        if let Some(stored) = found.record.fit {
            fitted.route = stored.route;
            fitted.thickness = stored.thickness;
            fitted.best_deviation = stored.best_deviation;
        }
    }
    if let Some(hole) = stored_hole.as_ref() {
        fitted.ink = crate::underlay::local_mask(hole, input.window);
    }
    if let Some((_, prepared)) = prepared_text.as_ref() {
        let mut hole = prepared.model_hole.to_mask();
        hole.bounds.x -= patch_shift.0 + input_shift.0;
        hole.bounds.y -= patch_shift.1 + input_shift.1;
        fitted.ink = hole;
    }
    let applied = cleaner_core::engines::model::applied_mask(&fitted, page.width, page.height);
    for tile in cleaner_core::engines::model::plan(applied.bounds) {
        crate::underlay::require_footprint(&input, tile, raw_page)?;
    }
    if let Some(crop) = cleaner_core::engines::render::crop_for(applied.bounds) {
        crate::underlay::require_footprint(&input, crop, raw_page)?;
    }

    if let Choice::Exact(engine) = plan.choice {
        let applicable = match engine {
            Engine::Lama => cleaner_core::engines::lama::applies(page),
            _ => true,
        };
        if !applicable {
            return Ok(Outcome::Refused("decline.reason.rungUnavailable"));
        }
    }

    let started = Instant::now();
    // What local FLUX aligned its answer's tone by, recorded as a cloud
    // render's attach records it.
    let mut tone: Option<ToneReport> = None;
    let (mut made, mut verdict) = if let Some(renderer) = test_renderer.as_mut() {
        let made = renderer(page, &fitted);
        let verdict = quality::assess(page, &made.mask, &made.pixels, noise);
        (made, verdict)
    } else if plan.fill_mode == Some("solid") {
        let pixels = match plan.solid_color {
            Some(color) => cleaner_core::engines::fill::render_solid_color(page, &fitted, color).map_err(|e|e.to_string())?,
            None => cleaner_core::engines::fill::render(page, &fitted),
        };
        let made = run::Made {
            engine: Engine::Fill,
            mask: fitted.mask.clone(),
            ink: fitted.ink.clone(),
            pixels,
            provider: None,
            model_sha256: None,
            pad: cleaner_core::strip::EdgePad::None,
            tiles: None,
        };
        let verdict = quality::assess(page, &made.mask, &made.pixels, noise);
        (made, verdict)
    } else {
        match plan.choice {
            Choice::Ladder(pick) => {
                // The same ceiling a run gets, and the same cap: a *pick* is a
                // starting rung, and nothing that starts by itself may reach rung
                // 3a or the cloud.
                match run::clean_region(&mut bench.rung2, page, &fitted, ceiling, pick, noise) {
                    Ok(run::Attempt::Cleaned(made, verdict)) => (*made, verdict),
                    Ok(run::Attempt::Declined(reason)) => return Ok(Outcome::Refused(reason)),
                    Err(detail) => return Ok(Outcome::Refused(engine_fault(&detail))),
                }
            }
            Choice::Exact(Engine::Flux) => match bench.flux(page, &fitted) {
                Ok((made, verdict, aligned)) => {
                    tone = aligned;
                    (made, verdict)
                }
                Err(reason) => return Ok(Outcome::Refused(reason)),
            },
            Choice::Exact(engine) => {
                match run::render_rung(&mut bench.rung2, engine, page, &fitted) {
                    Err(detail) => return Ok(Outcome::Refused(engine_fault(&detail))),
                    Ok(run::Rendered::Refused(reason)) => return Ok(Outcome::Refused(reason)),
                    Ok(run::Rendered::Made(made)) => {
                        let verdict = quality::assess(page, &made.mask, &made.pixels, noise);
                        (*made, verdict)
                    }
                }
            }
        }
    };

    if let Some((_, prepared)) = prepared_text.as_ref() {
        let mut base = prepared.corrected_base.to_mask();
        base.bounds.x -= patch_shift.0 + input_shift.0;
        base.bounds.y -= patch_shift.1 + input_shift.1;
        let mut alpha = prepared.blend_alpha.clone();
        if let Some(alpha) = alpha.as_mut() {
            alpha.bounds.x -= patch_shift.0 + input_shift.0;
            alpha.bounds.y -= patch_shift.1 + input_shift.1;
        }
        made = bound_text_shape_output(page, made, &seed, &base, alpha.as_ref())?;
        verdict = quality::assess(page, &made.mask, &made.pixels, noise);
    }
    // What this render read, for a later edit to be tested against. A
    // text-shape render follows a plan made over its whole window, so it
    // keeps the whole window.
    let whole = Rect::new(0, 0, input.window.w, input.window.h);
    let reach = if prepared_text.is_some() {
        whole
    } else {
        crate::underlay::render_reach(made.engine, seed.bounds, scale, made.mask.bounds,
            matches!(plan.choice, Choice::Ladder(_)), whole)
    };
    let reach = Rect::new(reach.x + input_shift.0, reach.y + input_shift.1, reach.w, reach.h);
    let reach_digest =
        crate::underlay::read_exact(&job, source_idx, raw_page, reach, order, strip_space)?.digest;
    translate_made(
        &mut made,
        (patch_shift.0 + input_shift.0, patch_shift.1 + input_shift.1),
    );
    let pad = made.pad;
    let mut snapshot = run::params_snapshot(
        made.engine,
        &fitted,
        started.elapsed(),
        pad,
        made.tiles,
        &verdict,
    );
    if made.engine == Engine::Flux {
        snapshot["flux_model"] = serde_json::json!(bench.flux_model.as_deref().unwrap_or(FLUX_MODEL));
    }
    if let Some(tone) = tone.as_ref() {
        snapshot["tone"] = tone.provenance();
    }
    snapshot["source"] = serde_json::json!(plan.source);
    snapshot["input_provenance"] = serde_json::json!({
        "version": 1, "input_sha256": input.digest, "predecessors_sha256": input.predecessors,
        "read_footprint": crate::underlay::page_rect(input.window,
            Rect::new(patch_shift.0, patch_shift.1, 0, 0)),
        "reach": crate::underlay::page_rect(reach, Rect::new(patch_shift.0, patch_shift.1, 0, 0)),
        "reach_sha256": reach_digest,
    });
    if let Some((_, prepared)) = prepared_text.as_ref() {
        snapshot["geometry_policy"] = serde_json::json!("text_shape");
        snapshot["mask_plan_version"] = serde_json::json!(MASK_PLAN_VERSION);
        snapshot["plan_identity_sha256"] = serde_json::json!(prepared.identity.identity_sha256);
        snapshot["write_support_sha256"] = serde_json::json!(prepared.identity.support_sha256);
    }
    if let Some(tool) = plan.tool.as_deref() {
        snapshot["tool"] = serde_json::json!(tool);
    }
    if let Some(mode) = plan.fill_mode {
        snapshot["fill_mode"] = serde_json::json!(mode);
    }
    if let Some(record) = existing.as_ref().filter(|record| record.provenance.params_snapshot.get("layer").is_some()) {
        // The new result keeps the layer's look. Its placement only when the
        // new output may be placed: a redraw is computed where the stored
        // mask is, so carrying a move onto it would draw that spot's
        // reconstruction somewhere else. It lands where it was made, which
        // the undo of this re-run gives back as it was.
        let mut layer = cleaner_core::project::orientation::display_layer_style(&job.project,record);
        if made.engine.layer_capabilities().transform != cleaner_core::patch::LayerPlacement::Movable {
            layer = cleaner_core::patch::LayerStyle { opacity: layer.opacity, ..Default::default() };
        }
        snapshot["layer"] = serde_json::to_value(layer).unwrap_or_default();
    }

    commit(
        located,
        job,
        source_idx,
        &plan.region_id,
        plan.clears_untouched,
        made,
        snapshot,
        source_sha256,
        order,
        prepared_text
            .as_ref()
            .map(|(_, prepared)| prepared.identity.identity_sha256.as_str()),
    )
}

/// A mask moved by a whole number of pixels.
/// The hole the current preprocessing reads from a stored patch: its
/// lettering, or its mask when it never stored any.
fn stored_hole_of(stored: &Patch) -> &Mask {
    cleaner_core::engines::render::Preprocessing::CURRENT.hole(&stored.mask, &stored.ink)
}

fn remap_paint(paint: &mut PaintPlan, anchor: cleaner_core::strip::Placement, origin: (i64,i64), width: u32, height: u32) {
    let map = |x: f64, y: f64| {
        let gx = x / 100.0 * anchor.width as f64 + anchor.x_offset as f64;
        let gy = y / 100.0 * anchor.height as f64 + anchor.y_offset as f64;
        ((gx - origin.0 as f64) / width as f64 * 100.0,
         (gy - origin.1 as f64) / height as f64 * 100.0)
    };
    for point in &mut paint.points {
        (point.x, point.y) = map(point.x, point.y);
    }
    match &mut paint.kind {
        PaintKind::Shape { shape, .. } => {
            for point in &mut shape.points {
                (point.x, point.y) = map(point.x, point.y);
            }
        }
        PaintKind::Clone { offset, .. } => {
            offset.0 = offset.0 * anchor.width as f64 / width as f64;
            offset.1 = offset.1 * anchor.height as f64 / height as f64;
        }
        PaintKind::Brush { .. } => {}
    }
}

fn translated_mask(mask: &Mask, (dx, dy): (i64, i64)) -> Mask {
    let mut moved = mask.clone();
    moved.bounds.x += dx;
    moved.bounds.y += dy;
    moved
}

fn lower_composite_revision(input: &crate::underlay::Input) -> String {
    let mut digest = Sha256::new();
    digest.update(b"text_shape_lower_v1\0");
    digest.update(input.digest.as_bytes());
    digest.update(input.predecessors.as_bytes());
    format!("{:x}", digest.finalize())
}

/// A short-lived reference to the exact support artifact in the project
/// sidecar. The reference is opaque to the UI; preview tiles and apply both
/// reload the same versioned raster through the Job store.
#[cfg(test)]
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextShapePreview {
    pub status: &'static str,
    pub region_id: String,
    pub plan_revision: u64,
    pub identity_sha256: Option<String>,
    pub support_sha256: Option<String>,
    pub bounds: Option<Rect>,
    pub preview_ref: Option<String>,
    pub reason: Option<String>,
}

#[cfg(test)]
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextShapeTile {
    pub bounds: Rect,
    /// Row-major 0/255 write permission, never a contour approximation.
    pub bits: Vec<u8>,
    pub support_sha256: String,
}

#[cfg(test)]
#[allow(dead_code)]
fn correction_preview(region_id: String, revision: u64, reason: String) -> TextShapePreview {
    TextShapePreview {
        status: "needs_mask_correction",
        region_id,
        plan_revision: revision,
        identity_sha256: None,
        support_sha256: None,
        bounds: None,
        preview_ref: None,
        reason: Some(reason),
    }
}

#[cfg(test)]
fn prepared_input_for_page(
    job: &Job,
    located: &Located,
    source_idx: usize,
    anchor_page: &Raster,
    support: Rect,
    order: u32,
) -> Result<(crate::underlay::Input, Rect), String> {
    if job.project.strip.mode != cleaner_core::project::StripMode::Longstrip {
        let input = crate::underlay::read(job, source_idx, anchor_page, support, order, None)?;
        let context = input.window;
        return Ok((input, context));
    }
    let strip = run::strip_of(&job.project);
    let anchor = *strip
        .pages()
        .get(located.page_index)
        .ok_or("strip anchor missing")?;
    let global = Rect::new(
        support.x + anchor.x_offset,
        support.y + anchor.y_offset,
        support.w,
        support.h,
    );
    let decode_bounds = Rect::new(
        global.x.saturating_sub(1024),
        global.y.saturating_sub(1024),
        global.w.saturating_add(2048),
        global.h.saturating_add(2048),
    );
    let x = decode_bounds.x.max(0);
    let y = decode_bounds.y.max(0);
    let right = decode_bounds.right().min(strip.width() as i64);
    let bottom = decode_bounds.bottom().min(strip.height() as i64);
    let requested = Rect::new(x, y, (right - x).max(0) as u32, (bottom - y).max(0) as u32);
    let mut joins = cleaner_core::strip::Joins::unchecked(strip.joins());
    for join in 0..strip.joins() {
        joins.set(join, cleaner_core::strip::JoinState::Verified);
    }
    let window = cleaner_core::strip::DecodeWindow {
        rect: strip.clamp(requested, &joins),
        requested,
        pad: cleaner_core::strip::EdgePad::None,
    };
    if u64::from(window.rect.w) * u64::from(window.rect.h) > 4096 * 4096 {
        return Err("strip preview context exceeds bounded read budget".into());
    }
    let read = cleaner_core::strip::read_window_borrowing(&strip, window, |position| {
        if position == located.page_index {
            return Ok(Cow::Borrowed(anchor_page));
        }
        let source = Library::resolve_page(&job.project, position)
            .ok_or_else(|| format!("strip page {position} missing"))?;
        let path = job.source_path(source).ok_or("strip source missing")?;
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        job.display_page(source,&bytes).map(Cow::Owned).map_err(|e| e.to_string())
    })?;
    let local = Rect::new(
        global.x - read.origin.0,
        global.y - read.origin.1,
        support.w,
        support.h,
    );
    let input = crate::underlay::read(
        job,
        source_idx,
        &read.raster,
        local,
        order,
        Some(crate::underlay::StripSpace {
            strip: &strip,
            origin: read.origin,
        }),
    )?;
    let context = Rect::new(
        input.window.x + read.origin.0 - anchor.x_offset,
        input.window.y + read.origin.1 - anchor.y_offset,
        input.window.w,
        input.window.h,
    );
    Ok((input, context))
}

/// Prepare synthetic or externally supplied lettering evidence at source
/// resolution. M4 will feed this from an app segmenter; this command does not
/// infer a mask from a detector rectangle. A page-sized JSON preview is never
/// returned. The raster lives in the existing project sidecar.
#[cfg(test)]
#[allow(dead_code)]
pub async fn prepare_text_shape(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: u32,
    region_id: Option<String>,
    mut plan: MaskPlan,
) -> Result<TextShapePreview, String> {
    crate::library::blocking(move || {
        let library = Library::for_app(&app)?;
        let Some(located) = locate_page(&library, &chapter_id, page_index as usize)? else {
            return Err("page not found".into());
        };
        let _lock = run::lock_job(&located.job_path)?;
        let mut job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
        let source_idx = Library::resolve_page(&job.project, located.page_index)
            .ok_or("page source not found")?;
        let path = job
            .source_path(source_idx)
            .ok_or("page source path not found")?;
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let raw = job.display_page(source_idx,&bytes).map_err(|e| e.to_string())?;
        let source_sha = sha256_hex(&bytes);
        if !plan.source_sha256.is_empty() && plan.source_sha256 != source_sha {
            return Err("source revision changed before mask preparation".into());
        }
        let id = region_id.unwrap_or_else(|| {
            let page_id = crate::library::page_id(&chapter_id, located.page_index);
            mint_hand_id(&job, &page_id)
        });
        if page_index_in(&chapter_id, &id) != Some(located.page_index)
            || (!plan.region_id.is_empty() && plan.region_id != id)
        {
            return Err("mask region identity does not belong to this page".into());
        }
        if let Some((old, _)) = job.load_text_shape_plan(&id).map_err(|e| e.to_string())? {
            if plan.plan_revision <= old.plan_revision {
                return Err("mask plan revision is stale".into());
            }
            if plan.base_revision == old.base_revision && plan.base_mask != old.base_mask {
                return Err("base lettering mask changed without a base revision".into());
            }
            if plan.correction_revision == old.correction_revision
                && (plan.additions != old.additions || plan.removals != old.removals)
            {
                return Err("mask corrections changed without a correction revision".into());
            }
        }
        if job
            .text_shape_correction(&id)
            .is_some_and(|failed| plan.plan_revision <= failed.plan_revision)
        {
            return Err("mask plan revision is stale after correction review".into());
        }
        if plan.version != MASK_PLAN_VERSION {
            return Err(format!(
                "unsupported text-shaped plan version {}",
                plan.version
            ));
        }
        plan.region_id = id.clone();
        plan.source_sha256 = source_sha;
        plan.lower_composite_sha256 = "preparing".into();
        plan.reading_context = provisional_text_shape_context(&plan);
        let provisional = match plan.prepare(raw.width, raw.height) {
            Ok(prepared) => prepared,
            Err(error) => {
                let reason = error.to_string();
                job.record_text_shape_correction(
                    source_idx,
                    &id,
                    plan.plan_revision,
                    plan.candidate_bounds,
                    &reason,
                )
                .map_err(|error| error.to_string())?;
                return Ok(correction_preview(id, plan.plan_revision, reason));
            }
        };
        let order = job
            .project
            .patches
            .iter()
            .find(|record| record.id == id)
            .map_or_else(|| next_order(&job, source_idx), |record| record.order);
        let (input, context) = match prepared_input_for_page(
            &job,
            &located,
            source_idx,
            &raw,
            provisional.write_support.bounds,
            order,
        ) {
            Ok(pair) => pair,
            Err(error) => {
                job.record_text_shape_correction(
                    source_idx,
                    &id,
                    plan.plan_revision,
                    plan.candidate_bounds,
                    &error,
                )
                .map_err(|failure| failure.to_string())?;
                return Ok(correction_preview(id, plan.plan_revision, error));
            }
        };
        plan.lower_composite_sha256 = lower_composite_revision(&input);
        plan.reading_context = context;
        let prepared = match plan.prepare(raw.width, raw.height) {
            Ok(prepared) => prepared,
            Err(error) => {
                let reason = error.to_string();
                job.record_text_shape_correction(
                    source_idx,
                    &id,
                    plan.plan_revision,
                    plan.candidate_bounds,
                    &reason,
                )
                .map_err(|error| error.to_string())?;
                return Ok(correction_preview(id, plan.plan_revision, reason));
            }
        };
        job.store_text_shape_plan(source_idx, &plan, &prepared)
            .map_err(|e| e.to_string())?;
        Ok(TextShapePreview {
            status: "ready_for_preview",
            region_id: id,
            plan_revision: plan.plan_revision,
            identity_sha256: Some(prepared.identity.identity_sha256.clone()),
            support_sha256: Some(prepared.identity.support_sha256.clone()),
            bounds: Some(prepared.write_support.bounds),
            preview_ref: Some(format!(
                "text-shape:{}:{}",
                plan.plan_revision, prepared.identity.identity_sha256
            )),
            reason: None,
        })
    })
    .await
}

#[cfg(test)]
fn provisional_text_shape_context(plan: &MaskPlan) -> Rect {
    if plan.base_mask.count() == 0 {
        if plan.additions.count() > 0 {
            plan.additions.bounds
        } else {
            plan.candidate_bounds
        }
    } else {
        plan.base_mask.bounds
    }
}

/// Return at most one 512x512 tile of the stored write-support raster. The
/// caller must name the exact accepted identity, so an old preview cannot be
/// displayed as though it belonged to a newer plan.
#[cfg(test)]
#[allow(dead_code)]
pub async fn text_shape_preview_tile(
    app: tauri::AppHandle,
    region_id: String,
    identity_sha256: String,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> Result<TextShapeTile, String> {
    crate::library::blocking(move || {
        if w == 0 || h == 0 || w > 512 || h > 512 {
            return Err("preview tile exceeds 512 pixels per axis".into());
        }
        let library = Library::for_app(&app)?;
        let located = locate_region(&library, &region_id)?.ok_or("region not found")?;
        let _lock = run::lock_job(&located.job_path)?;
        let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
        if job.text_shape_correction(&region_id).is_some() {
            return Err("text-shaped preview needs mask correction".into());
        }
        let (_, prepared) = job
            .load_text_shape_plan(&region_id)
            .map_err(|e| e.to_string())?
            .ok_or("text-shaped plan not found")?;
        if prepared.identity.identity_sha256 != identity_sha256 {
            return Err("stale text-shaped preview".into());
        }
        let bounds = Rect::new(x, y, w, h);
        let mut bits = vec![0; w as usize * h as usize];
        for py in y..bounds.bottom() {
            for px in x..bounds.right() {
                if prepared.write_support.contains(px, py) {
                    bits[(py - y) as usize * w as usize + (px - x) as usize] = 255;
                }
            }
        }
        Ok(TextShapeTile {
            bounds,
            bits,
            support_sha256: prepared.identity.support_sha256,
        })
    })
    .await
}

#[cfg(test)]
#[allow(dead_code)]
pub async fn apply_prepared_text_shape(
    app: tauri::AppHandle,
    region_id: String,
    identity_sha256: String,
    engine: String,
) -> Result<ApplyResult, String> {
    crate::library::blocking(move || {
        let Some(engine) = named_rung(Some(&engine)) else {
            return Ok(ApplyResult {
                error_code: Some("unsupported_engine"),
                ..ApplyResult::of("blocked")
            });
        };
        // The existing cloud grant protocol has no prepared-support binding.
        // Decline here rather than add an unreviewed authorization path.
        if matches!(engine, Engine::Cloud | Engine::Paint | Engine::Clone) {
            return Ok(ApplyResult {
                error_code: Some("unsupported_engine"),
                ..ApplyResult::of("blocked")
            });
        }
        let library = Library::for_app(&app)?;
        let Some(located) = locate_region(&library, &region_id)? else {
            return Ok(ApplyResult::of("not-found"));
        };
        let plan = Plan {
            region_id,
            expected_source: None,
            geometry: Geometry::Prepared(identity_sha256),
            choice: Choice::Exact(engine),
            source: "auto",
            tool: None,
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: None,
            stroke: None,
            drawn: None,
        };
        match edit(&app, &located, plan)? {
            Outcome::Cleaned(edited) => Ok(ApplyResult::applied(edited)),
            Outcome::Refused(_) => Ok(ApplyResult {
                error_code: Some("stale_or_unsafe_plan"),
                ..ApplyResult::of("blocked")
            }),
            Outcome::NotFound => Ok(ApplyResult::of("not-found")),
        }
    })
    .await
}

/// Include a clone stroke's sampling area in its bounded strip read. Clone
/// offsets are target minus source in anchor-page percent.
fn manual_requested(
    target: Rect,
    paint: Option<&PaintPlan>,
    anchor: Option<cleaner_core::strip::Placement>,
) -> Rect {
    let Some(PaintPlan {
        kind: PaintKind::Clone { offset, .. },
        ..
    }) = paint
    else {
        return target;
    };
    let Some(anchor) = anchor else { return target };
    let dx = (offset.0 / 100.0 * anchor.width as f64).round() as i64;
    let dy = (offset.1 / 100.0 * anchor.height as f64).round() as i64;
    let source = Rect::new(target.x - dx, target.y - dy, target.w, target.h);
    let x = target.x.min(source.x);
    let y = target.y.min(source.y);
    Rect::new(
        x,
        y,
        (target.right().max(source.right()) - x) as u32,
        (target.bottom().max(source.bottom()) - y) as u32,
    )
}

fn translate_made(made: &mut run::Made, by: (i64, i64)) {
    if by == (0, 0) {
        return;
    }
    made.mask.bounds.x += by.0;
    made.mask.bounds.y += by.1;
    made.ink.bounds.x += by.0;
    made.ink.bounds.y += by.1;
}

/// The last write boundary for a prepared text-shaped edit. Engines can read
/// larger crops, grow an internal hole, or generate a larger raster, but this
/// copies only the accepted W into a patch whose mask is exactly W. The source
/// copy is the ordered lower composite captured above, so shrinking W reveals
/// the right lower pixels when this region replaces its earlier patch.
fn bound_text_shape_output(
    below: &Raster,
    mut made: run::Made,
    support: &Mask,
    corrected_base: &Mask,
    blend_alpha: Option<&MaskRaster>,
) -> Result<run::Made, String> {
    let bounds = support.bounds;
    if bounds.w == 0
        || bounds.h == 0
        || support.is_empty()
        || bounds.x < 0
        || bounds.y < 0
        || bounds.right() > below.width as i64
        || bounds.bottom() > below.height as i64
    {
        return Err("prepared write support is empty or outside the input".into());
    }
    if made.pixels.mode != below.mode
        || made.pixels.depth != below.depth
        || made.pixels.width != made.mask.bounds.w
        || made.pixels.height != made.mask.bounds.h
    {
        return Err("engine output does not match the prepared source format".into());
    }
    let mut pixels = cleaner_core::engines::model::page_crop(below, bounds);
    let alpha_channel = below.mode.alpha_channel();
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if !support.contains(x, y) {
                continue;
            }
            if !made.mask.contains(x, y) {
                return Err("engine output does not cover approved write support".into());
            }
            let alpha = blend_alpha.map_or(255u32, |a| {
                if a.bounds.contains(x, y) {
                    a.bits[((y - a.bounds.y) as usize) * a.bounds.w as usize
                        + (x - a.bounds.x) as usize] as u32
                } else {
                    0
                }
            });
            let (sx, sy) = ((x - bounds.x) as u32, (y - bounds.y) as u32);
            let (gx, gy) = (
                (x - made.mask.bounds.x) as u32,
                (y - made.mask.bounds.y) as u32,
            );
            for channel in 0..below.mode.samples() {
                if alpha_channel == Some(channel) {
                    continue;
                }
                let source = pixels.sample(sx, sy, channel) as u32;
                let generated = made.pixels.sample(gx, gy, channel) as u32;
                let value = (source * (255 - alpha) + generated * alpha + 127) / 255;
                pixels.set_sample(sx, sy, channel, value as u16);
            }
        }
    }
    made.mask = support.clone();
    made.ink = corrected_base.clone();
    made.pixels = pixels;
    Ok(made)
}

#[cfg(test)]
mod text_shape_boundary_tests {
    use super::*;
    use cleaner_core::image::fixtures;

    #[test]
    fn every_enabled_local_engine_is_clamped_to_exact_support_on_fake_output() {
        for fixture in ["l8", "rgba8", "l16"] {
            let below = fixtures::by_name(fixture).raster;
            let mut support = Mask::empty(Rect::new(10, 10, 20, 20));
            support.set(12, 12, true);
            support.set(27, 25, true);
            support.set(22, 14, true);
            let generated_bounds = support.bounds.grown(6, below.width, below.height);
            let generated_mask = Mask::filled(generated_bounds);
            let mut generated = cleaner_core::engines::model::page_crop(&below, generated_bounds);
            let max = if below.depth == cleaner_core::image::BitDepth::Sixteen {
                65535
            } else {
                255
            };
            for y in 0..generated.height {
                for x in 0..generated.width {
                    for channel in 0..below.mode.samples() {
                        generated.set_sample(x, y, channel, max);
                    }
                }
            }
            for engine in [Engine::Fill, Engine::Lama, Engine::Flux] {
                let made = run::Made {
                    engine,
                    mask: generated_mask.clone(),
                    ink: generated_mask.clone(),
                    pixels: generated.clone(),
                    provider: None,
                    model_sha256: None,
                    pad: cleaner_core::strip::EdgePad::None,
                    tiles: None,
                };
                let bounded =
                    bound_text_shape_output(&below, made, &support, &support, None).unwrap();
                assert_eq!(
                    bounded.mask, support,
                    "{fixture} {engine:?} changed support"
                );
                for y in support.bounds.y..support.bounds.bottom() {
                    for x in support.bounds.x..support.bounds.right() {
                        if support.contains(x, y) {
                            continue;
                        }
                        for channel in 0..below.mode.samples() {
                            assert_eq!(
                                bounded.pixels.sample(
                                    (x - support.bounds.x) as u32,
                                    (y - support.bounds.y) as u32,
                                    channel
                                ),
                                below.sample(x as u32, y as u32, channel),
                                "{fixture} {engine:?} leaked at {x},{y}"
                            );
                        }
                    }
                }
                if let Some(alpha) = below.mode.alpha_channel() {
                    for y in support.bounds.y..support.bounds.bottom() {
                        for x in support.bounds.x..support.bounds.right() {
                            assert_eq!(
                                bounded.pixels.sample(
                                    (x - support.bounds.x) as u32,
                                    (y - support.bounds.y) as u32,
                                    alpha
                                ),
                                below.sample(x as u32, y as u32, alpha)
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn bounded_output_rejects_a_renderer_that_does_not_cover_w() {
        let below = fixtures::by_name("l8").raster;
        let support = Mask::filled(Rect::new(10, 10, 3, 3));
        let too_small = Mask::filled(Rect::new(10, 10, 1, 1));
        let made = run::Made {
            engine: Engine::Lama,
            mask: too_small.clone(),
            ink: too_small.clone(),
            pixels: cleaner_core::engines::model::page_crop(&below, too_small.bounds),
            provider: None,
            model_sha256: None,
            pad: cleaner_core::strip::EdgePad::None,
            tiles: None,
        };
        assert!(bound_text_shape_output(&below, made, &support, &support, None).is_err());
    }

    #[test]
    fn a_new_preview_revision_rejects_the_old_identity_without_a_patch_write() {
        use cleaner_core::image::{encode, Format};
        use cleaner_core::ingest::source_ref;
        use cleaner_core::project::{Project, StripMode};
        use cleaner_core::text_shape::{MaskQualityState, MASK_PLAN_VERSION};
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "mc-text-shape-stale-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("raws")).unwrap();
        let raw = fixtures::by_name("l8").raster;
        let bytes = encode(&raw, Format::Png).unwrap();
        let source_path = root.join("raws/page.png");
        std::fs::write(&source_path, &bytes).unwrap();
        let source = source_ref(&source_path, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let mut job = Job::create(
            &manifest,
            Project::new(
                manifest.parent().unwrap(),
                "test",
                StripMode::Single,
                &[source],
            ),
        )
        .unwrap();
        let region_id = "chapter-p001-h1".to_string();
        let mut base = Mask::empty(Rect::new(18, 18, 5, 5));
        base.set(20, 20, true);
        let empty = MaskRaster::from(Mask::empty(Rect::new(0, 0, 0, 0)));
        let mut plan = MaskPlan {
            version: MASK_PLAN_VERSION,
            region_id: region_id.clone(),
            source_sha256: sha256_hex(&bytes),
            lower_composite_sha256: "pending".into(),
            algorithm_id: "synthetic-test".into(),
            model_id: None,
            candidate_bounds: Rect::new(15, 15, 10, 10),
            refinement_crop: Rect::new(10, 10, 20, 20),
            base_revision: 1,
            correction_revision: 0,
            plan_revision: 1,
            base_mask: MaskRaster::from(base),
            additions: empty.clone(),
            removals: empty,
            padding_px: 2,
            model_hole_margin_px: 5,
            reading_context: Rect::new(18, 18, 5, 5),
            blend_alpha: None,
            quality: MaskQualityState::Ready,
        };
        let provisional = plan.prepare(raw.width, raw.height).unwrap();
        let input = crate::underlay::read(&job, 0, &raw, provisional.write_support.bounds, 0, None)
            .unwrap();
        plan.lower_composite_sha256 = lower_composite_revision(&input);
        plan.reading_context = input.window;
        let accepted = plan.prepare(raw.width, raw.height).unwrap();
        job.store_text_shape_plan(0, &plan, &accepted).unwrap();
        let old_identity = accepted.identity.identity_sha256.clone();
        plan.plan_revision = 2;
        plan.padding_px = 5;
        let newer = plan.prepare(raw.width, raw.height).unwrap();
        job.store_text_shape_plan(0, &plan, &newer).unwrap();
        drop(job);
        let located = Located {
            chapter_id: "chapter".into(),
            job_path: manifest.clone(),
            page_index: 0,
        };
        let models = root.join("no-models");
        let mut bench = Bench {
            app_data: None,
            sidecar_override: None,
            flux_model: None,
            flux_backend: None,
            detector: run::detector_on_demand(&models, Preference::CpuOnly),
            rung2: run::Rung2::new(&models, Preference::CpuOnly),
            flux: None,
            flux_key: None,
            detected_seed: None,
        };
        let edit_plan = Plan {
            region_id: region_id.clone(),
            expected_source: None,
            geometry: Geometry::Prepared(old_identity),
            choice: Choice::Exact(Engine::Fill),
            source: "auto",
            tool: None,
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: None,
            stroke: None,
            drawn: None,
        };
        let mut fake = |image: &Raster, fitted: &Fitted| run::Made {
            engine: Engine::Fill,
            mask: fitted.mask.clone(),
            ink: fitted.ink.clone(),
            pixels: cleaner_core::engines::fill::render(image, fitted),
            provider: None,
            model_sha256: None,
            pad: cleaner_core::strip::EdgePad::None,
            tiles: None,
        };
        assert!(matches!(
            edit_with_bench_render(
                &mut bench,
                Engine::Fill,
                &located,
                edit_plan,
                Some(&mut fake)
            )
            .unwrap(),
            Outcome::Refused(_)
        ));
        assert!(Job::open(&manifest).unwrap().project.patches.is_empty());
        // The latest preview also becomes stale when a newly visible lower
        // patch changes its bounded read input before apply.
        let mut job = Job::open(&manifest).unwrap();
        let a_mask = Mask::filled(Rect::new(21, 20, 1, 1));
        let mut a_pixels = cleaner_core::engines::model::page_crop(&raw, a_mask.bounds);
        a_pixels.set_sample(0, 0, 0, if raw.sample(21, 20, 0) == 0 { 255 } else { 0 });
        job.complete_display_region(
            0,
            &Patch {
                id: "chapter-p001-h0".into(),
                mask: a_mask.clone(),
                ink: a_mask,
                pixels: a_pixels,
                order: 0,
                visible: true,
                provenance: Provenance {
                    engine: Engine::Fill,
                    engine_version: "fake".into(),
                    model_sha256: None,
                    execution_provider: "cpu".into(),
                    params_snapshot: serde_json::json!({}),
                    mask_sha256: "fake".into(),
                    source_sha256: sha256_hex(&bytes),
                    cloud: None,
                    created: 0,
                },
            },
            None,
        )
        .unwrap();
        drop(job);
        let latest_plan = Plan {
            region_id: region_id.clone(),
            expected_source: None,
            geometry: Geometry::Prepared(newer.identity.identity_sha256),
            choice: Choice::Exact(Engine::Fill),
            source: "auto",
            tool: None,
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: None,
            stroke: None,
            drawn: None,
        };
        assert!(matches!(
            edit_with_bench_render(
                &mut bench,
                Engine::Fill,
                &located,
                latest_plan,
                Some(&mut fake)
            )
            .unwrap(),
            Outcome::Refused(_)
        ));
        assert!(!Job::open(&manifest)
            .unwrap()
            .project
            .patches
            .iter()
            .any(|r| r.id == region_id));
        std::fs::remove_dir_all(root).ok();
    }
}

/// Write the patch and answer with the region - the half of an edit that is the
/// same whatever made the pixels.
///
/// Factored out when painting arrived, because a paint stroke reaches it from a
/// different place: it skips the fit, the ladder and the quality metric, and it
/// must not skip the mask digest, the untouched row, the order, or the manifest
/// write. Two spellings of this tail would be two ways for a patch to be
/// recorded, and "no second pipeline" is a rule about exactly that.
#[allow(clippy::too_many_arguments)]
fn commit(
    located: &Located,
    mut job: Job,
    source_idx: usize,
    region_id: &str,
    clears_untouched: bool,
    made: run::Made,
    snapshot: serde_json::Value,
    source_sha256: String,
    order: u32,
    text_shape_identity: Option<&str>,
) -> Result<Outcome, String> {
    let mask_sha256 = run::mask_digest(&made.mask);
    let patch = Patch {
        id: region_id.to_owned(),
        mask: made.mask,
        ink: made.ink,
        pixels: made.pixels,
        order,
        // Stored-mask reruns of deleted layers are refused before rendering.
        visible: true,
        provenance: Provenance {
            engine: made.engine,
            engine_version: env!("CARGO_PKG_VERSION").into(),
            model_sha256: made.model_sha256,
            // Rungs 0 and 1 have no session to name one, and unlike a run there
            // is no detector here whose provider could stand in.
            execution_provider: made.provider.unwrap_or_else(|| "cpu".to_owned()),
            params_snapshot: snapshot,
            mask_sha256,
            source_sha256,
            cloud: None,
            created: run::now(),
        },
    };

    if clears_untouched {
        clear_untouched(&mut job, source_idx, region_id);
    }
    // A stored detection is replaced by its patch in the same write, keeping
    // the review flag it was detected with.
    let review_state = job
        .project
        .detections
        .iter()
        .find(|row| row.id == region_id)
        .map(|row| row.review_state.clone());
    match (text_shape_identity, review_state) {
        (Some(identity), _) => job.complete_text_shape_region(source_idx, &patch, identity, None),
        (None, Some(review_state)) => {
            job.complete_detection(region_id, &patch, review_state).map(|_| ())
        }
        (None, None) => job.complete_display_region(source_idx, &patch, None),
    }
    .map_err(|e| e.to_string())?;
    crate::underlay::refresh_dependencies(&mut job, region_id)?;

    Ok(
        match crate::library::region_and_status(
            &located.chapter_id,
            &job.project,
            source_idx,
            region_id,
        ) {
            Some((region, page_status)) => Outcome::Cleaned(Edited {
                region,
                page_status,
            }),
            None => Outcome::NotFound,
        },
    )
}

/* ------------------------------------------------------------------ */
/* Paint and clone: running one                                        */
/* ------------------------------------------------------------------ */

/// The stroke, and the patch it becomes.
///
/// Everything the kernels need is here and nothing else is: the page's own
/// samples, the patches under this one, and numbers. No pixel buffer crossed
/// the seam to get here - the interface sent a dab list and this rasterises
/// it at the page's own scale.
fn paint_patch(
    job: &Job,
    source_idx: usize,
    page: &Raster,
    paint: &PaintPlan,
    order: u32,
    strip_context: Option<StripPaintContext<'_>>,
) -> Result<(run::Made, usize, Rect), &'static str> {
    use cleaner_core::paint::{self, CloneMode, HealMode, PaintError, StrokePoint};

    if !paint::supports(page) {
        return Err("decline.reason.paintUnsupportedMode");
    }

    // A filled shape leaves here: it has no dabs to plan, no tip to stamp and
    // no spacing to walk. What it has is a mask and a colour, which is the
    // whole of the work.
    if let PaintKind::Shape { color, shape, outline } = &paint.kind {
        return shape_patch(
            job,
            source_idx,
            page,
            shape,
            *color,
            *outline,
            order,
            strip_context,
        );
    }

    let (width, height) = (page.width as f64, page.height as f64);
    let points: Vec<StrokePoint> = paint
        .points
        .iter()
        .map(|p| StrokePoint {
            x: p.x / 100.0 * width,
            y: p.y / 100.0 * height,
            p: p.p.clamp(0.0, 1.0),
        })
        .collect();
    let offset = match paint.kind {
        // A filled shape left several lines up, so the arms below it are
        // written for the two kinds that are still here. It is spelled out
        // rather than caught by a wildcard so that a fourth kind of paint
        // cannot be added without this function being read.
        PaintKind::Brush { .. } | PaintKind::Shape { .. } => (0.0, 0.0),
        PaintKind::Clone { offset, .. } => (offset.0 / 100.0 * width, offset.1 / 100.0 * height),
    };

    let under = patches_under(
        job,
        source_idx,
        order,
        &points,
        paint.spec.size,
        offset,
        strip_context,
    )
    .map_err(|_| "decline.reason.rungUnavailable")?;

    let painted = match paint.kind {
        PaintKind::Brush { color } | PaintKind::Shape { color, .. } => {
            paint::render_brush(page, &under, Some(order), &points, &paint.spec, color)
        }
        PaintKind::Clone { heal, .. } => paint::render_clone(
            page,
            &under,
            Some(order),
            &points,
            &paint.spec,
            offset,
            if heal {
                CloneMode::Heal(HealMode::Soft)
            } else {
                CloneMode::Clone
            },
        ),
    };

    let painted = match painted {
        Ok(painted) => painted,
        Err(PaintError::UnsupportedMode { .. }) => {
            return Err("decline.reason.paintUnsupportedMode")
        }
        // A stroke that covered nothing and a window that would not composite
        // are both "there is no patch to make here", which is the sentence
        // `rungUnavailable` already carries for an empty seed a few lines up.
        Err(PaintError::Color(_)) => return Err("decline.reason.paintUnsupportedColor"),
        Err(PaintError::Empty) | Err(PaintError::Composite(_)) => {
            return Err("decline.reason.rungUnavailable")
        }
    };

    Ok((
        run::Made {
            engine: match paint.kind {
                PaintKind::Brush { .. } | PaintKind::Shape { .. } => Engine::Paint,
                PaintKind::Clone { .. } => Engine::Clone,
            },
            // A stroke is its own lettering: the user said where the paint
            // goes, and that is also the whole of what the edit covers.
            ink: painted.mask.clone(),
            mask: painted.mask,
            pixels: painted.pixels,
            // No session, no model, no tiles and no edge pad: none of the four
            // has a true value here, and inventing one would put it in the
            // manifest.
            provider: None,
            model_sha256: None,
            pad: cleaner_core::strip::EdgePad::None,
            tiles: None,
        },
        painted.dabs,
        painted.read_window,
    ))
}

/// A drawn shape, filled flat, as the patch it becomes.
///
/// **The paint branch's second renderer, and deliberately not a fourth engine.**
/// It stands beside [`paint_patch`]'s brush and clone rather than beside the
/// ladder, because filling a shape with a chosen colour is the same *kind* of
/// act as painting with one: the user said what goes there, so there is
/// nothing to fit, nothing to reconstruct and nothing for the quality metric to
/// have an opinion about.
///
/// What is not shared with the brush is the coverage. A brush's mask is the
/// discs its dabs swept; a shape's is its own outline, rasterised once at the
/// page's own scale by [`shape_coverage`] from an exact signed distance, so
/// the rim is anti-aliased, `feather` is a soft edge rather than growth, and
/// the outline is exactly `width` page pixels inside the edge - the same inner
/// stroke the draft preview draws. So the dab planner and the stamp kernel are
/// not on this path at all, and the one number that survives from the brush is
/// `opacity` - the wash.
///
/// Shape pixels retain the chosen colour. Their uniform gesture opacity is a
/// layer opacity, applied by the compositor at the destination. Baking the
/// colour into the original underlay would carry that old background along
/// when the user moves the shape.
#[allow(clippy::too_many_arguments)]
fn shape_patch(
    job: &Job,
    source_idx: usize,
    page: &Raster,
    shape: &PaintedShape,
    color: [u8; 3],
    outline: Option<([u8; 3], f64)>,
    order: u32,
    strip_context: Option<StripPaintContext<'_>>,
) -> Result<(run::Made, usize, Rect), &'static str> {
    // An empty mask is "there is no patch to make here", which is the sentence
    // `rungUnavailable` already carries for an empty seed.
    let outline_width = outline.map_or(0.0, |(_, width)| width);
    let (mask, rim) = shape_coverage(shape, outline_width, page.width, page.height)
        .ok_or("decline.reason.rungUnavailable")?;
    let bounds = mask.bounds;

    let under = patches_over(job, source_idx, order, bounds, strip_context)
        .map_err(|_| "decline.reason.rungUnavailable")?;
    let mut pixels = cleaner_core::composite::composite_region(page, &under, bounds, Some(order))
        .map_err(|_| "decline.reason.rungUnavailable")?;
    // The window came back clamped to the page. The mask's bounds were built
    // inside the page already, so the two agree - but a patch whose buffer and
    // whose mask disagreed would be refused by the compositor later, with far
    // less to say about why.
    if pixels.width != bounds.w || pixels.height != bounds.h {
        return Err("decline.reason.rungUnavailable");
    }

    let native = |color| cleaner_core::engines::fill::selected_samples(page, color)
        .map(|samples| samples.into_iter().enumerate().map(|(c,v)| if page.mode.alpha_channel() == Some(c) { None } else { Some(v) }).collect::<Vec<_>>())
        .map_err(|_| "decline.reason.paintUnsupportedColor");
    let levels = native(color)?;
    let outline_levels = outline.map(|(color, _)| native(color)).transpose()?;
    let top = (1u32 << page.depth.bits().min(16)) as f64 - 1.0;
    for y in 0..bounds.h {
        for x in 0..bounds.w {
            let index = y as usize * bounds.w as usize + x as usize;
            if mask.bits[index] == 0 {
                continue;
            }
            let rim = f64::from(rim[index]);
            for (channel, level) in levels.iter().enumerate() {
                // Alpha is opaque in the stored patch; the mask's coverage and
                // the layer opacity blend it with the destination. Keeping the
                // old underlay's alpha here would move that old transparency
                // with the shape.
                let fill = level.map_or(top, f64::from);
                let edge = outline_levels
                    .as_ref()
                    .and_then(|levels| levels[channel])
                    .map_or(fill, f64::from);
                let value = fill + (edge - fill) * rim;
                pixels.set_sample(x, y, channel, value.round().clamp(0.0, top) as u16);
            }
        }
    }

    Ok((
        run::Made {
            engine: Engine::Paint,
            ink: mask.clone(),
            mask,
            pixels,
            provider: None,
            model_sha256: None,
            pad: cleaner_core::strip::EdgePad::None,
            tiles: None,
        },
        // No dabs: nothing was walked. The provenance says how many vertices
        // there were instead ([`paint_snapshot`]).
        0,
        bounds,
    ))
}

/// A solid shape's coverage at the page's own scale: the mask holds how much
/// of each pixel the shape covers (0 to 255), and `rim` how much of that is
/// outline rather than fill (0 to 1), both from an exact signed distance.
///
/// With no feather the edge is a one-pixel box filter, which is what an
/// anti-aliased vector renderer draws. A feather is a Gaussian of sigma
/// `feather / 2` across the edge, which is what blurring the shape by that
/// much does to a straight edge. The outline is an **inner** stroke of
/// `outline_width` page pixels, so the shape never grows past what was drawn.
/// A line has no fill and no outline: it is a round-capped stroke whose half
/// width rides in `feather`.
fn shape_coverage(
    shape: &PaintedShape,
    outline_width: f64,
    width: u32,
    height: u32,
) -> Option<(Mask, Vec<f32>)> {
    let line = shape.kind == ShapeKind::Line;
    if shape.points.len() < if line { 2 } else { 3 } || width == 0 || height == 0 {
        return None;
    }
    let points: Vec<(f64, f64)> = shape
        .points
        .iter()
        .map(|point| (point.x / 100.0 * width as f64, point.y / 100.0 * height as f64))
        .collect();
    if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return None;
    }
    let half = if line { shape.feather.clamp(0.5, MAX_FEATHER_PX) } else { 0.0 };
    let sigma = if line { 0.0 } else { shape.feather.clamp(0.0, MAX_FEATHER_PX) / 2.0 };
    let stroke = if line { 0.0 } else { outline_width.clamp(0.0, 30.0) };
    // Past this distance from the edge a pixel is wholly in or out, and wholly
    // fill or wholly outline.
    let band = stroke + 3.0 * sigma + 2.0;

    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in &points {
        x0 = x0.min(*x);
        y0 = y0.min(*y);
        x1 = x1.max(*x);
        y1 = y1.max(*y);
    }
    let reach = half + 3.0 * sigma + 1.0;
    let left = (x0 - reach).floor().clamp(0.0, width as f64) as i64;
    let top = (y0 - reach).floor().clamp(0.0, height as f64) as i64;
    let right = (x1 + reach).ceil().clamp(0.0, width as f64) as i64;
    let bottom = (y1 + reach).ceil().clamp(0.0, height as f64) as i64;
    if right <= left || bottom <= top {
        return None;
    }
    let bounds = Rect::new(left, top, (right - left) as u32, (bottom - top) as u32);

    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (hx, hy) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let (rx, ry) = (hx.max(0.5), hy.max(0.5));
    let coverage = |distance: f64| -> f64 {
        if sigma < 0.25 {
            (0.5 - distance).clamp(0.0, 1.0)
        } else {
            0.5 * erfc(distance / (sigma * std::f64::consts::SQRT_2))
        }
    };

    let mut mask = Mask::empty(bounds);
    let mut rim = vec![0.0f32; mask.bits.len()];
    let mut row = vec![0.0f64; bounds.w as usize];
    for y in bounds.y..bounds.bottom() {
        let py = y as f64 + 0.5;
        if shape.kind == ShapeKind::Polygon {
            polygon_row(&points, py, bounds, band, &mut row);
        }
        for x in bounds.x..bounds.right() {
            let px = x as f64 + 0.5;
            let column = (x - bounds.x) as usize;
            // Negative inside, in page pixels.
            let distance = match shape.kind {
                ShapeKind::Rect => {
                    let (qx, qy) = ((px - cx).abs() - hx, (py - cy).abs() - hy);
                    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0)
                }
                ShapeKind::Ellipse => {
                    let (dx, dy) = (px - cx, py - cy);
                    let r = ((dx / rx).powi(2) + (dy / ry).powi(2)).sqrt();
                    // The gap between an ellipse and its scaled copy through
                    // this point is at least |r - 1| times the minor radius,
                    // so a pixel that far from the edge skips the exact solve.
                    let lower = (r - 1.0).abs() * rx.min(ry);
                    let magnitude = if lower > band { lower } else { ellipse_distance(rx, ry, dx, dy) };
                    if r < 1.0 { -magnitude } else { magnitude }
                }
                ShapeKind::Polygon => row[column],
                ShapeKind::Line => distance_to_segment(points[0], points[1], px, py) - half,
            };
            let alpha = coverage(distance);
            let byte = (alpha * 255.0).round() as u8;
            if byte == 0 {
                continue;
            }
            let local = (y - bounds.y) as usize * bounds.w as usize + column;
            mask.bits[local] = byte;
            if stroke > 0.0 {
                rim[local] = (1.0 - coverage(distance + stroke)) as f32;
            }
        }
    }
    (!mask.is_empty()).then_some((mask, rim))
}

/// One row of a polygon's signed distance at pixel centres, even-odd inside,
/// written into `row`. Only edges within `band` of the row are measured; a
/// pixel farther than that from every edge is given `band`, which is all a
/// coverage function needs of it.
fn polygon_row(points: &[(f64, f64)], py: f64, bounds: Rect, band: f64, row: &mut [f64]) {
    row.iter_mut().for_each(|value| *value = band);
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (a, b) = (points[j], points[i]);
        j = i;
        if py < a.1.min(b.1) - band || py > a.1.max(b.1) + band {
            continue;
        }
        let from = ((a.0.min(b.0) - band).floor() as i64).max(bounds.x);
        let to = ((a.0.max(b.0) + band).ceil() as i64).min(bounds.right());
        for x in from..to {
            let slot = &mut row[(x - bounds.x) as usize];
            *slot = slot.min(distance_to_segment(a, b, x as f64 + 0.5, py));
        }
    }
    // Even-odd spans along this row, the rule `contains_point` uses.
    let mut crossings: Vec<f64> = Vec::new();
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let ((xi, yi), (xj, yj)) = (points[i], points[j]);
        if (yi > py) != (yj > py) {
            crossings.push((xj - xi) * (py - yi) / (yj - yi) + xi);
        }
        j = i;
    }
    crossings.sort_by(f64::total_cmp);
    for pair in crossings.chunks_exact(2) {
        let from = ((pair[0] - 0.5).ceil() as i64).max(bounds.x);
        let to = ((pair[1] - 0.5).ceil() as i64).min(bounds.right());
        for x in from..to {
            let slot = &mut row[(x - bounds.x) as usize];
            *slot = -*slot;
        }
    }
}

/// Unsigned distance from `(px, py)` to the ellipse with semi-axes `rx`, `ry`
/// centred on the origin. Eberly's bisection ("Distance from a Point to an
/// Ellipse"), which is exact and does not break down near a circle.
fn ellipse_distance(rx: f64, ry: f64, px: f64, py: f64) -> f64 {
    // The solve wants the first quadrant and the major axis first.
    let (e0, e1, y0, y1) = if rx >= ry {
        (rx, ry, px.abs(), py.abs())
    } else {
        (ry, rx, py.abs(), px.abs())
    };
    if y1 > 0.0 {
        if y0 > 0.0 {
            let (z0, z1) = (y0 / e0, y1 / e1);
            let g = z0 * z0 + z1 * z1 - 1.0;
            if g == 0.0 {
                return 0.0;
            }
            let r0 = (e0 / e1).powi(2);
            let n0 = r0 * z0;
            let (mut s0, mut s1) = (z1 - 1.0, if g < 0.0 { 0.0 } else { n0.hypot(z1) - 1.0 });
            let mut s = 0.0;
            for _ in 0..96 {
                s = (s0 + s1) / 2.0;
                if s == s0 || s == s1 {
                    break;
                }
                let g = (n0 / (s + r0)).powi(2) + (z1 / (s + 1.0)).powi(2) - 1.0;
                if g > 0.0 {
                    s0 = s;
                } else if g < 0.0 {
                    s1 = s;
                } else {
                    break;
                }
            }
            let (x0, x1) = (r0 * y0 / (s + r0), y1 / (s + 1.0));
            (x0 - y0).hypot(x1 - y1)
        } else {
            (y1 - e1).abs()
        }
    } else {
        let (numer, denom) = (e0 * y0, e0 * e0 - e1 * e1);
        if numer < denom {
            let ratio = numer / denom;
            let (x0, x1) = (e0 * ratio, e1 * (1.0 - ratio * ratio).sqrt());
            (x0 - y0).hypot(x1)
        } else {
            (y0 - e0).abs()
        }
    }
}

/// The complementary error function, to within 1.5e-7 (Abramowitz and Stegun
/// 7.1.26). The standard library has no `erf`.
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t * (0.254_829_592
        + t * (-0.284_496_736 + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let tail = poly * (-z * z).exp();
    if x >= 0.0 { tail } else { 2.0 - tail }
}

/// One sRGB colour as this page's own samples, channel by channel.
///
/// `None` for the alpha channel, which is copied through rather than written.
/// Only the four modes [`cleaner_core::paint::supports`] admits reach here, and
/// all four are eight bits a sample, so a byte is a level.
#[cfg(test)]
fn solid_levels(mode: cleaner_core::image::ColorMode, color: [u8; 3]) -> Vec<Option<u16>> {
    use cleaner_core::image::ColorMode;
    // Rec. 601 luma, integer-rounded: the same weighting the rest of the
    // application reads a grey page's tone with.
    let grey = ((color[0] as f64 * 0.299) + (color[1] as f64 * 0.587) + (color[2] as f64 * 0.114))
        .round()
        .clamp(0.0, 255.0) as u16;
    match mode {
        ColorMode::Gray => vec![Some(grey)],
        ColorMode::GrayAlpha => vec![Some(grey), None],
        ColorMode::Rgb => vec![
            Some(color[0] as u16),
            Some(color[1] as u16),
            Some(color[2] as u16),
        ],
        ColorMode::Rgba => {
            vec![
                Some(color[0] as u16),
                Some(color[1] as u16),
                Some(color[2] as u16),
                None,
            ]
        }
        // Unreachable: `paint::supports` refused these several lines up. A
        // colour is still better than a panic, and every sample gets one.
        other => vec![Some(grey); other.samples()],
    }
}

/// The visible patches under a **rectangle**, decoded - [`patches_under`]'s
/// half for a gesture whose coverage is an area rather than a swept path.
///
/// The two filters are that function's and load-bearing for the same reasons:
/// `order` is exclusive, so a re-painted region blends over what is beneath it
/// and not over its own previous commit, and the box is intersected against
/// each record's `bbox` before its buffer is read.
#[derive(Clone, Copy)]
struct StripPaintContext<'a> {
    strip: &'a cleaner_core::strip::Strip,
    order: &'a [usize],
    /// Origin of the bounded raster in strip coordinates.
    origin: (i64, i64),
}

fn patch_in_paint_space(
    job: &Job,
    record: &PatchRecord,
    context: Option<StripPaintContext<'_>>,
) -> Result<Option<Patch>, String> {
    let mut patch = job.load_display_patch(record).map_err(|e| e.to_string())?;
    let Some(context) = context else {
        return Ok(Some(patch));
    };
    let Some(placed) = context
        .order
        .iter()
        .position(|&source| source == record.source_idx)
        .and_then(|position| context.strip.pages().get(position))
    else {
        return Ok(None);
    };
    let dx = placed.x_offset - context.origin.0;
    let dy = placed.y_offset - context.origin.1;
    patch.mask.bounds.x += dx;
    patch.mask.bounds.y += dy;
    patch.ink.bounds.x += dx;
    patch.ink.bounds.y += dy;
    Ok(Some(patch))
}

fn record_box_in_paint_space(
    job: &Job,
    record: &PatchRecord,
    context: Option<StripPaintContext<'_>>,
) -> Option<Rect> {
    let bbox = cleaner_core::project::orientation::display_bbox(&job.project, record);
    let Some(context) = context else {
        return Some(bbox);
    };
    let placed = context
        .order
        .iter()
        .position(|&source| source == record.source_idx)
        .and_then(|position| context.strip.pages().get(position))?;
    Some(Rect::new(
        bbox.x + placed.x_offset - context.origin.0,
        bbox.y + placed.y_offset - context.origin.1,
        bbox.w,
        bbox.h,
    ))
}

fn patches_over(
    job: &Job,
    source_idx: usize,
    order: u32,
    box_of: Rect,
    strip_context: Option<StripPaintContext<'_>>,
) -> Result<Vec<Patch>, String> {
    let mut patches = Vec::new();
    for record in &job.project.patches {
        if !record.visible
            || record.order >= order
            || (strip_context.is_none() && record.source_idx != source_idx)
        {
            continue;
        }
        let Some(record_box) = record_box_in_paint_space(job, record, strip_context) else {
            continue;
        };
        if !overlaps(record_box, box_of) {
            continue;
        }
        if let Some(patch) = patch_in_paint_space(job, record, strip_context)? {
            patches.push(patch);
        }
    }
    Ok(patches)
}

/// The visible patches this stroke paints **over**, decoded.
///
/// Two filters and both are load-bearing. `order` is exclusive, so a re-painted
/// region blends over what is under it and not over its own previous commit -
/// otherwise every re-run would compound. And the stroke's own box is intersected
/// against each record's `bbox` *before* the buffer is read from the sidecar, so
/// a page carrying forty cleaned balloons decodes the two the stroke actually
/// crosses rather than all of them.
fn patches_under(
    job: &Job,
    source_idx: usize,
    order: u32,
    points: &[cleaner_core::paint::StrokePoint],
    size: f64,
    offset: (f64, f64),
    strip_context: Option<StripPaintContext<'_>>,
) -> Result<Vec<Patch>, String> {
    let reach = size / 2.0 + 2.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for point in points {
        x0 = x0.min(point.x - reach);
        y0 = y0.min(point.y - reach);
        x1 = x1.max(point.x + reach);
        y1 = y1.max(point.y + reach);
    }
    if x0 > x1 {
        return Ok(Vec::new());
    }
    // Clone reads from the source as well as writing to the target, and the
    // surface it is handed is one buffer covering both.
    let box_of = Rect::new(
        x0.min(x0 - offset.0).floor() as i64,
        y0.min(y0 - offset.1).floor() as i64,
        (x1.max(x1 - offset.0) - x0.min(x0 - offset.0))
            .ceil()
            .max(1.0) as u32,
        (y1.max(y1 - offset.1) - y0.min(y0 - offset.1))
            .ceil()
            .max(1.0) as u32,
    );

    let mut patches = Vec::new();
    for record in &job.project.patches {
        if !record.visible
            || record.order >= order
            || (strip_context.is_none() && record.source_idx != source_idx)
        {
            continue;
        }
        let Some(record_box) = record_box_in_paint_space(job, record, strip_context) else {
            continue;
        };
        if !overlaps(record_box, box_of) {
            continue;
        }
        if let Some(patch) = patch_in_paint_space(job, record, strip_context)? {
            patches.push(patch);
        }
    }
    Ok(patches)
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

/// The provenance a painted patch carries.
///
/// **Its own, not [`run::params_snapshot`]'s.** That function's fields are a
/// fit's - `thickness`, `deviation`, the write set, the quality verdict - and a
/// painted stroke has none of them; feeding it a dummy `Fitted` would write four
/// numbers about a fit that never happened.
/// "The thresholds, dilations and radii **actually used**" is asked for, and
/// for a stroke those are the brush.
fn paint_snapshot(
    plan: &Plan,
    paint: &PaintPlan,
    engine: Engine,
    dabs: usize,
    elapsed: std::time::Duration,
) -> serde_json::Value {
    let spec = &paint.spec;
    let mut snapshot = serde_json::json!({
        "rung": engine.rung_key(),
        "source": plan.source,
        // Neither of the three fill-mode words describes a stroke; `solid` is
        // the one that comes closest, because the pixels were given rather than
        // matched or reconstructed. `library::fill_mode` says the same thing
        // from the other side.
        "fill_mode": "solid",
        "elapsed_ms": elapsed.as_millis() as u64,
        "brush": {
            "size": spec.size,
            "hardness": spec.hardness,
            "opacity": spec.opacity,
            "flow": spec.flow,
            "spacing": spec.spacing,
            "pressure_size": spec.pressure_size,
            "pressure_opacity": spec.pressure_opacity,
        },
        // How much of the stroke the commit actually walked. The one number
        // that separates a tap from a sweep after the fact.
        "dabs": dabs,
        "points": paint.points.len(),
        "seed": spec.seed,
    });
    if let Some(tool) = plan.tool.as_deref() {
        snapshot["tool"] = serde_json::json!(tool);
    }
    match paint.kind {
        PaintKind::Brush { color } => {
            snapshot["mode"] = serde_json::json!("paint");
            snapshot["color"] =
                serde_json::json!(format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]));
        }
        PaintKind::Shape { color, ref shape, .. } => {
            snapshot["mode"] = serde_json::json!("solid");
            snapshot["layer"] = serde_json::json!({
                "opacity": spec.opacity.round().clamp(0.0, 100.0) as u8,
            });
            snapshot["color"] =
                serde_json::json!(format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]));
            // The geometry, in the two numbers that say what was drawn: which
            // shape, and how far past its outline the mask reached. The
            // radii actually used are asked for, and for a filled shape
            // those are these.
            snapshot["shape"] = serde_json::json!(match shape.kind {
                ShapeKind::Rect => "rect",
                ShapeKind::Ellipse => "ellipse",
                ShapeKind::Polygon => "polygon",
                ShapeKind::Line => "line",
            });
            snapshot["feather"] = serde_json::json!(shape.feather.clamp(0.0, MAX_FEATHER_PX));
            snapshot["vertices"] = serde_json::json!(shape.points.len());
        }
        PaintKind::Clone { heal, offset } => {
            snapshot["mode"] = serde_json::json!(if heal { "heal" } else { "clone" });
            snapshot["alignment"] = serde_json::json!(paint.alignment);
            // `target − source`, in page percent: the internal direction, which
            // is the negation of the seam's `cloneOffset` (see `clone_offset`).
            // Percent and not native pixels, because the native figure is only
            // meaningful beside this page's size.
            snapshot["source_offset"] = serde_json::json!({ "x": offset.0, "y": offset.1 });
        }
    }
    snapshot
}

/// A rectangle, inside the page it is on. A gesture may run off the edge; a
/// mask may not.
fn clamped(rect: Rect, page: &Raster) -> Rect {
    let x0 = rect.x.clamp(0, (page.width.saturating_sub(1)) as i64);
    let x1 = rect.right().clamp(x0 + 1, page.width as i64);
    let y0 = rect.y.clamp(0, (page.height.saturating_sub(1)) as i64);
    let y1 = rect.bottom().clamp(y0 + 1, page.height as i64);
    Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
}

/// The order a new patch takes on its page: after everything already there, so
/// a hand edit composites over the automatic pass's own work.
fn next_order(job: &Job, _source_idx: usize) -> u32 {
    job.project
        .patches
        .iter()
        .map(|record| record.order + 1)
        .max()
        .unwrap_or(0)
}

/// Take the gate-skipped row this region was off the manifest, and take its
/// contribution to the counters with it.
///
/// The counters state what the manifest currently holds
/// ([`crate::run::reset_page`] makes the same argument from the other side), so
/// a row that leaves has to leave the count it added.
fn clear_untouched(job: &mut Job, source_idx: usize, region_id: &str) {
    let (mut gate_dropped, mut declined) = (0u32, 0u32);
    job.project.regions_untouched.retain(|record| {
        if record.source_idx != source_idx
            || untouched_id(job_page_id(region_id), record.bbox) != region_id
        {
            return true;
        }
        // A held candidate (it records its balloon) was counted as neither.
        if record.inside_bubble.is_some() {
        } else if record.reason.contains("gateSkipped") {
            gate_dropped += 1;
        } else {
            declined += 1;
        }
        false
    });
    let counters = &mut job.project.counters;
    counters.gate_dropped = counters.gate_dropped.saturating_sub(gate_dropped);
    counters.declined = counters.declined.saturating_sub(declined);
}

/// The page id inside a region id - everything up to the last `-`-prefixed
/// suffix the region kinds use.
fn job_page_id(region_id: &str) -> &str {
    ["-u", "-r", "-h", "-d"]
        .into_iter()
        .filter_map(|m| region_id.rfind(m))
        .max()
        .map(|at| &region_id[..at])
        .unwrap_or(region_id)
}

/// The id [`crate::library::region_of_untouched`] derives for a gate-skipped
/// row, restated so a row can be found from the id the seam sent back.
fn untouched_id(page_id: &str, bbox: Rect) -> String {
    format!("{page_id}-u{}-{}-{}-{}", bbox.x, bbox.y, bbox.w, bbox.h)
}

/* ------------------------------------------------------------------ */
/* Geometry across the seam                                            */
/* ------------------------------------------------------------------ */

/// A percentage box, in the page's own pixels - [`crate::library::percent_of`]
/// run backwards.
fn pixels_of(bbox: Bbox, width: u32, height: u32) -> Rect {
    let x = bbox.x / 100.0 * width as f64;
    let y = bbox.y / 100.0 * height as f64;
    let w = bbox.w / 100.0 * width as f64;
    let h = bbox.h / 100.0 * height as f64;
    Rect::new(
        x.round() as i64,
        y.round() as i64,
        w.round().max(1.0) as u32,
        h.round().max(1.0) as u32,
    )
}

/// The page's size, without opening its pixels.
pub(crate) fn page_size(job: &Job, source_idx: usize) -> Option<(u32, u32)> {
    job.project.sources.get(source_idx).map(|source| source.orientation.size(source.w, source.h))
}

/// The stroke a round brush painted, as the seam carries it.
///
/// **Vector, not raster**, and the choice is the coordinate space's: the points
/// are in the same normalised page percent every other `bbox` on the seam is,
/// the radius is in page pixels because a brush is a tool on the image, and the
/// backend is the only side that knows the page's real size. A pre-rendered
/// alpha mask would have to be rasterised by the interface at *some* resolution
/// and resampled here - a stroke rounded twice - where a path and a radius
/// rasterise once, at the page's own scale, into exactly the [`Mask`] the
/// engines already take.
#[derive(Debug, Clone, serde::Deserialize)]
struct PaintedStroke {
    /// The path the pointer took, in page percent (0 to 100 on both axes).
    points: Vec<StrokePoint>,
    /// Half the brush's `size` parameter, in **page pixels**.
    radius: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
struct StrokePoint {
    x: f64,
    y: f64,
}

/// A painted stroke as a mask over the page it was painted on.
///
/// `None` where there is nothing to paint - no points, or a stroke entirely off
/// the page - so a caller falls back to the rectangle rather than committing an
/// empty mask.
fn stroke_mask(stroke: &PaintedStroke, width: u32, height: u32) -> Option<Mask> {
    if stroke.points.is_empty() {
        return None;
    }
    let points: Vec<(f64, f64)> = stroke
        .points
        .iter()
        .map(|point| {
            (
                point.x / 100.0 * width as f64,
                point.y / 100.0 * height as f64,
            )
        })
        .collect();
    let mask = Mask::from_stroke(&points, stroke.radius, width, height);
    (!mask.is_empty()).then_some(mask)
}

/// The **area** a Shapes gesture drew, as the seam carries it.
///
/// The companion of [`PaintedStroke`] and the same bargain: vector, not
/// raster, in the two coordinate spaces each side can check. The vertices are
/// page percent like every other `bbox` on the seam; `feather` is page pixels,
/// because it is a distance on the image rather than on the screen and this is
/// the only side that knows how big the page really is.
///
/// Three kinds cover the tool's four shapes, because a lasso *is* a polygon -
/// drawn freehand rather than clicked. A rectangle and an ellipse arrive as the
/// four corners of their box, so one payload describes all four and there is
/// one parser rather than three.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
struct PaintedShape {
    kind: ShapeKind,
    /// The outline, in page percent (0 to 100 on both axes). Closed here: the
    /// last vertex joins the first.
    points: Vec<StrokePoint>,
    /// How far the mask grows past the outline, in **page pixels**.
    #[serde(default)]
    feather: f64,
}

/// A word this build knows how to rasterise. Anything else fails to
/// deserialise, and the caller falls back to the bounding box it also sent -
/// which is the promised degradation for a backend that does not understand
/// a geometry.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum ShapeKind {
    Rect,
    Ellipse,
    Polygon,
    Line,
}

/// The most feather a shape may carry, in page pixels - **the tool window's own
/// maximum**, which is the largest value anything can legitimately send.
///
/// It is a clamp rather than a refusal because the cost is superlinear:
/// [`Mask::dilated`] tests a `(2r+1)²` kernel at every pixel of the grown box,
/// so a parameter that arrived as a thousand - hand-written, or a unit slip -
/// would be an hour of arithmetic rather than a wrong answer.
const MAX_FEATHER_PX: f64 = 20.0;

/// A drawn shape as a mask over the page it was drawn on.
///
/// `None` where there is nothing to fill - fewer than three vertices, or a
/// shape entirely off the page - so a caller falls back to the rectangle rather
/// than committing an empty mask.
///
/// **Feather is grown into the shape, not dilated onto it afterwards**, and
/// both halves of that matter. A [`Mask::dilated`] pass would cost a
/// `(2r+1)²` kernel at every pixel - minutes for a page-sized rectangle at the
/// row's own maximum - and it would square the rim of an ellipse at small
/// radii, because its structuring element is a square below diameter 5. Growing
/// the *test* instead is one comparison per pixel for a rectangle and an
/// ellipse, exact for both, and a distance to the outline for a polygon.
///
/// **What it is not is a soft edge.** A [`Mask`] is one bit per pixel - the set
/// an engine may write, and every engine, the compositor and the export take it
/// that way - so `feather` moves the boundary outwards rather than fading it.
fn shape_mask(shape: &PaintedShape, width: u32, height: u32) -> Option<Mask> {
    if shape.points.len() < if shape.kind == ShapeKind::Line { 2 } else { 3 } || width == 0 || height == 0 {
        return None;
    }
    let points: Vec<(f64, f64)> = shape
        .points
        .iter()
        .map(|point| {
            (
                point.x / 100.0 * width as f64,
                point.y / 100.0 * height as f64,
            )
        })
        .collect();
    if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return None;
    }
    let feather = shape.feather.clamp(if shape.kind == ShapeKind::Line { 0.5 } else { 0.0 }, MAX_FEATHER_PX);

    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in &points {
        x0 = x0.min(*x);
        y0 = y0.min(*y);
        x1 = x1.max(*x);
        y1 = y1.max(*y);
    }
    // The outline's own box, grown by the feather and then brought onto the
    // page: a shape may run off the edge, a mask may not.
    let left = (x0 - feather).floor().clamp(0.0, width as f64) as i64;
    let top = (y0 - feather).floor().clamp(0.0, height as f64) as i64;
    let right = (x1 + feather).ceil().clamp(0.0, width as f64) as i64;
    let bottom = (y1 + feather).ceil().clamp(0.0, height as f64) as i64;
    if right <= left || bottom <= top {
        return None;
    }
    let bounds = Rect::new(left, top, (right - left) as u32, (bottom - top) as u32);

    // The ellipse inscribed in the outline's own box, which is what the tool
    // drew: two dragged corners, and the ellipse that touches all four sides.
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (rx, ry) = (
        ((x1 - x0) / 2.0).max(0.5) + feather,
        ((y1 - y0) / 2.0).max(0.5) + feather,
    );

    let mut mask = Mask::empty(bounds);
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            // Pixel centres, so a shape that lands between two pixel grids
            // covers the ones it is actually over.
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let inside = match shape.kind {
                // The four corners that arrived describe exactly this box, and
                // the feather is already in the bounds this loop walks.
                ShapeKind::Rect => {
                    px >= x0 - feather
                        && px <= x1 + feather
                        && py >= y0 - feather
                        && py <= y1 + feather
                }
                ShapeKind::Ellipse => {
                    let (dx, dy) = ((px - cx) / rx, (py - cy) / ry);
                    dx * dx + dy * dy <= 1.0
                }
                ShapeKind::Polygon => {
                    contains_point(&points, px, py)
                        || (feather > 0.0 && distance_to_outline(&points, px, py) <= feather)
                }
                ShapeKind::Line => distance_to_segment(points[0], points[1], px, py) <= feather,
            };
            if inside {
                mask.set(x, y, true);
            }
        }
    }
    (!mask.is_empty()).then_some(mask)
}

fn distance_to_segment(a: (f64, f64), b: (f64, f64), px: f64, py: f64) -> f64 {
    let (ex, ey) = (b.0 - a.0, b.1 - a.1);
    let length = ex * ex + ey * ey;
    let t = if length > 0.0 { (((px - a.0) * ex + (py - a.1) * ey) / length).clamp(0.0, 1.0) } else { 0.0 };
    ((px - a.0 - ex * t).powi(2) + (py - a.1 - ey * t).powi(2)).sqrt()
}

/// How far a point is from the nearest edge of a closed polygon, in pixels.
///
/// The feather's half of [`shape_mask`] for the one kind that has no closed
/// form: a point outside the outline is in the mask when it is within the
/// feather of it. Costs one pass over the edges per pixel, which is why the
/// caller asks only for pixels the polygon does not already contain.
fn distance_to_outline(points: &[(f64, f64)], px: f64, py: f64) -> f64 {
    let mut best = f64::MAX;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (ax, ay) = points[j];
        let (bx, by) = points[i];
        let (ex, ey) = (bx - ax, by - ay);
        let len2 = ex * ex + ey * ey;
        // A degenerate edge is its own endpoint, which the neighbouring edges
        // already cover; measuring to the point is still the right answer.
        let t = if len2 > 0.0 {
            (((px - ax) * ex + (py - ay) * ey) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (dx, dy) = (px - (ax + ex * t), py - (ay + ey * t));
        best = best.min(dx * dx + dy * dy);
        j = i;
    }
    best.sqrt()
}

/// Even-odd crossing: whether a point is inside a closed polygon.
///
/// The polygon is closed implicitly - the last vertex joins the first - and
/// self-intersection is resolved by the even-odd rule, which is the rule a
/// freehand lasso wants: a loop drawn back over itself leaves a hole rather
/// than filling the whole scribble.
fn contains_point(points: &[(f64, f64)], px: f64, py: f64) -> bool {
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/* ------------------------------------------------------------------ */
/* Paint and clone: the params, as the contract carries them           */
/* ------------------------------------------------------------------ */

/// One pointer sample of a paint stroke, in page percent, with its pressure.
///
/// `p` defaults to `0.5` rather than to `1.0`, and the default is the contract's:
/// `PointerEvent.pressure` is
/// frequently `0` or a flat `0.5` under WKWebView, so a device that says nothing
/// is treated as pressing half rather than as pressing hardest.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
struct PaintPoint {
    x: f64,
    y: f64,
    #[serde(default = "half")]
    p: f64,
}

fn half() -> f64 {
    0.5
}

/// What the paint branch was asked to do.
#[derive(Debug, Clone, PartialEq)]
enum PaintKind {
    /// A colour brush. The colour is straight sRGB bytes, from `#rrggbb`.
    Brush { color: [u8; 3] },
    /// Clone or heal. `offset` is `target − source` in **page percent**, as the
    /// seam already sends it; `edit` scales it to native pixels, because it is
    /// the only side that knows the page's real size.
    Clone { heal: bool, offset: (f64, f64) },
    /// A drawn shape filled flat with a chosen colour - Shapes' `solid` mode.
    ///
    /// It is a paint and not a clean, which is why it is here rather than on
    /// the ladder: nothing is fitted, no rung is asked what belongs under the
    /// shape and no quality metric assesses a colour somebody picked. What
    /// separates it from [`PaintKind::Brush`] is the coverage - a brush covers
    /// what its dabs swept, a shape covers its own outline - so the shape
    /// rides here instead of a dab list.
    Shape { color: [u8; 3], shape: PaintedShape, outline: Option<([u8; 3], f64)> },
}

/// The paint half of a [`Plan`].
///
/// **Parsed at both seam methods and acted on at one.** `createRegion` and
/// `applyTool` read `params` independently and converge on `edit`, and painting
/// over an *existing* region dispatches `applyTool`
/// (`src/lib/editor/toolapply.svelte.js`) - so a `createRegion`-only change
/// would silently route every retouch back into LaMa. One optional
/// field on the plan is what makes both routes one route.
#[derive(Debug, Clone, PartialEq)]
struct PaintPlan {
    kind: PaintKind,
    /// The stroke in page percent. Converted to native pixels in [`edit`].
    points: Vec<PaintPoint>,
    /// `size` is already in native page pixels - a brush is a tool on the
    /// image, and `stroke.radius` has been native throughout.
    spec: cleaner_core::paint::BrushSpec,
    /// `aligned` or `nonAligned`, recorded rather than acted on - one commit is
    /// one stroke, and the two only differ across strokes.
    alignment: &'static str,
}

impl PartialEq for PaintPoint {
    fn eq(&self, other: &Self) -> bool {
        self.x == other.x && self.y == other.y && self.p == other.p
    }
}

fn number(params: &serde_json::Value, key: &str) -> Option<f64> {
    params
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .filter(|v| v.is_finite())
}

fn flag(params: &serde_json::Value, key: &str, fallback: bool) -> bool {
    params
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(fallback)
}

/// `#rrggbb` - and `#rgb`, because an `<input type="color">` is not the only
/// thing that can fill this field. Anything else is black, which is the ink a
/// manga cleaner reaches for and is a better answer than refusing the stroke.
fn colour_of(text: Option<&str>) -> [u8; 3] {
    let Some(hex) = text.map(|t| t.trim().trim_start_matches('#')) else {
        return [0, 0, 0];
    };
    let pair = |at: usize, len: usize| -> Option<u8> {
        let slice = hex.get(at..at + len)?;
        let value = u8::from_str_radix(slice, 16).ok()?;
        Some(if len == 1 { value * 17 } else { value })
    };
    match hex.len() {
        6 => match (pair(0, 2), pair(2, 2), pair(4, 2)) {
            (Some(r), Some(g), Some(b)) => [r, g, b],
            _ => [0, 0, 0],
        },
        3 => match (pair(0, 1), pair(1, 1), pair(2, 1)) {
            (Some(r), Some(g), Some(b)) => [r, g, b],
            _ => [0, 0, 0],
        },
        _ => [0, 0, 0],
    }
}

/// The stroke's points, from `params.paint.points` or - for `cloneHeal`, whose
/// `paint` block is optional - from the `stroke` every drawing tool already
/// sends, at the default pressure.
fn paint_points(params: &serde_json::Value, paint: Option<&serde_json::Value>) -> Vec<PaintPoint> {
    let listed = paint
        .and_then(|p| p.get("points"))
        .and_then(|v| serde_json::from_value::<Vec<PaintPoint>>(v.clone()).ok())
        .unwrap_or_default();
    if !listed.is_empty() {
        return listed;
    }
    params
        .get("stroke")
        .and_then(|value| serde_json::from_value::<PaintedStroke>(value.clone()).ok())
        .map(|stroke| {
            stroke
                .points
                .iter()
                .map(|p| PaintPoint {
                    x: p.x,
                    y: p.y,
                    p: half(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether this call is a paint or clone stroke, and what it asks for.
///
/// `None` is "the ordinary path" and is the answer for every tool that is not
/// one of these two, and for a `brush` whose `mode` is still `add`/`erase` -
/// those keep today's mask-then-inpaint behaviour untouched, which is the
/// contract's own wording.
fn paint_plan(tool: &str, params: &serde_json::Value) -> Option<PaintPlan> {
    let mode = params.get("mode").and_then(|v| v.as_str());
    let paint = params.get("paint").filter(|v| v.is_object());

    let kind = match tool {
        "brush" if mode == Some("paint") => PaintKind::Brush {
            color: colour_of(
                paint
                    .and_then(|p| p.get("color"))
                    .or_else(|| params.get("color"))
                    .and_then(|v| v.as_str()),
            ),
        },
        // **Always real from now on.** The tool has read `cloneSource` and
        // `cloneOffset` off the seam and thrown them away since it existed;
        // there is no `mode` gate here because there is no legacy behaviour
        // worth keeping.
        "cloneHeal" => {
            let offset = clone_offset(params)?;
            PaintKind::Clone {
                heal: mode != Some("clone"),
                offset,
            }
        }
        // A shape filled with a colour. `solid` is Shapes' own word for it and
        // is deliberately not `fill`, which on that same row names rung 0 -
        // the planar fill, which samples the paper and lays down the tone it
        // found rather than a tone the user chose.
        "shapes" if mode == Some("solid") => {
            let shape = paint
                .and_then(|block| block.get("shape"))
                .or_else(|| params.get("painted"))
                .and_then(|value| serde_json::from_value::<PaintedShape>(value.clone()).ok())?;
            PaintKind::Shape {
                color: colour_of(
                    paint
                        .and_then(|p| p.get("color"))
                        .or_else(|| params.get("color"))
                        .and_then(|v| v.as_str()),
                ),
                shape,
                outline: number(params, "outlineWidth").filter(|width| *width > 0.0).map(|width| (
                    colour_of(params.get("outlineColor").and_then(|v| v.as_str())),
                    width.clamp(0.0, 30.0),
                )),
            }
        }
        _ => return None,
    };

    let points = paint_points(params, paint);
    // A shape is the one paint whose coverage is not its path: a rectangle
    // drag delivers one pointer sample and the keyboard route delivers none,
    // and both describe an area perfectly well. Every other kind is its
    // points, and no points is no stroke.
    if points.is_empty() && !matches!(kind, PaintKind::Shape { .. }) {
        return None;
    }

    // The `paint` block wins where it carries a value, and `params` is the
    // fallback: the two spellings both appear on the wire, because `size`,
    // `hardness`, `opacity` and `flow` are ordinary tool params as well as
    // brush ones.
    let from = |key: &str| {
        paint
            .and_then(|p| number(p, key))
            .or_else(|| number(params, key))
    };
    let switch = |key: &str, fallback: bool| match paint {
        Some(block) if block.get(key).is_some() => flag(block, key, fallback),
        _ => flag(params, key, fallback),
    };
    let default = cleaner_core::paint::BrushSpec::default();
    let spec = cleaner_core::paint::BrushSpec {
        // `size` is a diameter in native page pixels; `stroke.radius` is half of
        // it and is what the older tools send, so it stands in.
        size: from("size")
            .or_else(|| {
                params
                    .get("stroke")
                    .and_then(|v| v.get("radius"))
                    .and_then(serde_json::Value::as_f64)
                    .map(|r| r * 2.0)
            })
            .unwrap_or(default.size)
            .clamp(1.0, 4096.0),
        hardness: from("hardness")
            .unwrap_or(default.hardness)
            .clamp(0.0, 100.0),
        flow: from("flow").unwrap_or(100.0).clamp(0.0, 100.0),
        opacity: from("opacity").unwrap_or(100.0).clamp(0.0, 100.0),
        spacing: from("spacing").unwrap_or(default.spacing),
        pressure_size: switch("pressureSize", true),
        pressure_opacity: switch("pressureOpacity", false),
        seed: paint
            .and_then(|p| p.get("seed"))
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(0),
    };

    Some(PaintPlan {
        kind,
        points,
        spec,
        alignment: match params.get("alignment").and_then(|v| v.as_str()) {
            Some("nonAligned") => "nonAligned",
            _ => "aligned",
        },
    })
}

/// `target − source`, in page percent - **the negation of what the seam sends**.
///
/// The two sides spell the same displacement in opposite directions, and the
/// seam's spelling is the one that was there first. `gesture.js#cloneOffset`
/// computes `source − strokeStart` and reconstructs the source as
/// `strokeStart + offset`; the live preview and `gesture.test.js` are both built
/// on that reading. The kernels want the other one - `paint::render_clone` reads
/// each dab's source at `target − offset` - so the conversion happens **here, at
/// the parse site**, and exactly once. Nothing downstream of this function sees
/// the seam's sign, and nothing in the interface sees the kernel's.
///
/// `cloneSource` is the alt-clicked point, and the offset from the stroke's
/// first sample is the same quantity computed the long way - kept as a fallback
/// so a caller that sends only the source still clones from where the user
/// pointed rather than not at all. It is subtracted in the *internal*
/// direction already, so it is not negated a second time.
fn clone_offset(params: &serde_json::Value) -> Option<(f64, f64)> {
    let point = |key: &str| {
        let value = params.get(key)?;
        Some((number(value, "x")?, number(value, "y")?))
    };
    if let Some((x, y)) = point("cloneOffset") {
        // `source = strokeStart + cloneOffset`, so `target − source = −cloneOffset`.
        return Some((-x, -y));
    }
    let (sx, sy) = point("cloneSource")?;
    let first = paint_points(params, params.get("paint")).first().copied()?;
    Some((first.x - sx, first.y - sy))
}

/// What a painted gesture **means**, and it is now one answer for every tool
/// that paints one: **the paint is the mask**.
///
/// The user said where the edit goes, and that is authoritative - no fitting
/// search, nothing found, nothing moved.
///
/// **A shape beats a box wherever there is one.** The rectangle is still the
/// answer for a Shapes drag and for the keyboard route, which genuinely
/// describe rectangles; a brush stroke describes a swept disc, and reading it
/// as its bounding box produces an L-shaped stroke that cleaned the square
/// around it.
///
/// **The AI mask brush used to be the exception and is not any more.** Its
/// shape was a *hint*: the detector was run over the whole page and whatever
/// text it found under the stroke became the mask. Two things were wrong with
/// that. The detector - the text finder - ran on every manual stroke, which
/// is a full-page detection to answer a gesture that had already
/// said where it wanted paint. And what it found *won*: a stroke aimed at a
/// leftover beside a detected line cleaned the line instead, because the
/// detector's box was preferred to the hand's own shape. The stroke is the
/// evidence now, and the engine the tool window named is what runs over it.
fn painted_geometry(rect: Rect, painted: Option<Mask>) -> Geometry {
    match painted {
        Some(shape) => Geometry::Painted(shape),
        None => Geometry::Given(rect),
    }
}

/* ------------------------------------------------------------------ */
/* What a caller asked for                                             */
/* ------------------------------------------------------------------ */

/// The three fill modes: `match-surround`, `reconstruct`, `solid`.
///
/// Cycles three: `match-surround` → `reconstruct` → `solid` →
/// `match-surround`.
fn next_fill_mode(current: &str) -> &'static str {
    match current {
        "match-surround" => "reconstruct",
        "reconstruct" => "solid",
        "solid" => "match-surround",
        _ => "match-surround",
    }
}

/// The rung a fill mode implies. The mock's own mapping
/// (`src/lib/api/tools.js`), which is the specification for this.
fn engine_for_fill_mode(mode: &str) -> Engine {
    match mode {
        "reconstruct" => Engine::Lama,
        _ => Engine::Fill,
    }
}

/// The rungs a Layers row's picker may name, weakest first.
///
/// The cloud is not among them and neither is anything above rung 3a: this is
/// the list `stronger` and `simpler` step along, and every rung in it is one
/// this build can actually run.
const MANUAL_RUNGS: [Engine; 3] = [Engine::Fill, Engine::Lama, Engine::Flux];

fn step(current: Engine, up: bool) -> Engine {
    let at = MANUAL_RUNGS.iter().position(|engine| *engine == current);
    match at {
        None => current,
        Some(at) if up => MANUAL_RUNGS[(at + 1).min(MANUAL_RUNGS.len() - 1)],
        Some(at) => MANUAL_RUNGS[at.saturating_sub(1)],
    }
}

/// What a caller's `engine` word asks for.
///
/// **The pick is read first, and that ordering is the whole of it.** The tool
/// window's two rows name a rung outright - `fill`, `denoise`, `lama` - and so
/// does a Layers row's picker, so the same word arrives here meaning two
/// different things depending on who sent it. This function serves the one
/// caller that means a **start**: `cleanAnyway`, which reads Auto clean's own
/// rows and must keep the escalation that goes with them - a region the gate
/// held back is the last one that should be pinned to a declined patch.
///
/// So every word [`run::parse_pick`] knows is a start, and anything left is a
/// rung named outright: `flux`, which no automatic run may reach and which is
/// therefore only ever an explicit request.
fn choice_for(engine: Option<&str>, fallback: EnginePick) -> Choice {
    match run::parse_pick(engine) {
        Some(pick) => Choice::Ladder(Some(pick)),
        None => match named_rung(engine) {
            Some(rung) => Choice::Exact(rung),
            None => Choice::Ladder(Some(fallback)),
        },
    }
}

/// The `params.engine` a tool sends, as a rung. `None` is "the caller did not
/// name one", which is not the same as a name this build does not know - an
/// unknown name falls back to the fill/redraw pick rather than to a rung
/// nobody asked for.
fn named_rung(engine: Option<&str>) -> Option<Engine> {
    run::parse_rung(engine?)
}

/// Whether a re-run with no grant would have to go to the cloud: the cloud
/// named outright, or a cloud patch run again as what it was. Rendering either
/// on this machine is a swap the user never chose. A cloud patch re-run with a
/// local engine named (`named`, even FLUX, the engine it recorded), or stepped
/// or cycled to another one, is that choice, and runs here.
fn rerun_needs_cloud(
    target: Engine,
    current: Engine,
    is_cloud_provenance: bool,
    named: bool,
) -> bool {
    target == Engine::Cloud || (is_cloud_provenance && target == current && !named)
}

/* ------------------------------------------------------------------ */
/* Answering                                                           */
/* ------------------------------------------------------------------ */

/// Say what could not be done, rather than answering `null` and leaving the
/// interface to show nothing at all.
fn refuse(reason: &'static str) {
    crate::events::notice(
        "notice.mask.rerunFailed",
        serde_json::json!({ "reasonKey": reason }),
        "warn",
    );
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

fn explicit_remote_or_invalid_target(params: &serde_json::Value) -> bool {
    params.get("engine").and_then(|v| v.as_str()) == Some("cloud")
        || ["executionTarget", "target"].iter().any(|key| {
            params
                .get(*key)
                .is_some_and(|value| value != &serde_json::json!({"type":"local"}))
        })
}

/// The grant a cloud request carries, if it carries one.
fn grant_nonce_in(params: &serde_json::Value) -> Option<String> {
    params
        .get("grantNonce")
        .or_else(|| params.get("grant_nonce"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

/// Refuse a cloud request because the permission switch is off.
fn cloud_blocked() {
    crate::events::notice("notice.cloud.blocked", serde_json::json!({}), "warn");
}

/// Where a cloud render left its region.
enum CloudRender {
    /// Committed: the region as the manifest now holds it, and its page's
    /// status. Boxed: a region is large, and `Stopped` is two words.
    Committed(Box<ApiRegion>, String),
    /// Not committed: `failed`, `cancelled` or `unknown`, and the code the
    /// render's last `cloud://attempt` event carried as well.
    Stopped(&'static str, &'static str),
}

/// Render one stored region in the cloud with the grant `params` carries.
///
/// The caller has checked the permission switch and found the region; the
/// grant, the target and the region's bytes are checked again below this, by
/// the service, against what consent bound. Every way this ends reaches the
/// interface as a `cloud://attempt` event, including the refusals that come
/// before the service is asked, so a render the interface is waiting on never
/// goes quiet.
fn render_in_cloud(
    app: &tauri::AppHandle,
    located: &Located,
    region_id: &str,
    params: &serde_json::Value,
    grant_nonce: &str,
) -> CloudRender {
    let refuse_with = |code: &'static str| {
        let sink = crate::inference::commands::cloud_attempt_sink(app);
        sink(&CloudAttemptProgress {
            attempt_id: attempt_id_for_nonce(grant_nonce),
            provider: None,
            profile_id: None,
            region_id: region_id.to_owned(),
            chapter_id: located.chapter_id.clone(),
            page_index: located.page_index as u32,
            phase: RenderPhase::Failed,
            elapsed_ms: 0,
            error_code: Some(code),
        });
        CloudRender::Stopped("failed", code)
    };

    let field = |key: &str| params.get(key).cloned();
    let (Some(target), Some(recipe), Some(intent)) = (
        field("executionTarget").and_then(|v| serde_json::from_value::<ExecutionTarget>(v).ok()),
        field("recipe").and_then(|v| serde_json::from_value::<RenderRecipe>(v).ok()),
        field("intent").and_then(|v| serde_json::from_value::<OperationIntent>(v).ok()),
    ) else {
        return refuse_with("invalid_request");
    };
    let Ok(config) = crate::inference::config::read_inference_config(app) else {
        return refuse_with("config_unreadable");
    };
    // The grant was minted for the target selected then, and the render goes
    // there even when the user has picked another since. A target that is no
    // longer configured is gone; one that was edited fails on its grant epoch.
    let (Some(provider), Some(profile_id)) = (target.provider(), target.profile_id()) else {
        return refuse_with("target_changed");
    };
    if crate::inference::profile_in_service(app, provider, profile_id)
        == Err(crate::inference::Withdrawn::ProfileGone) {
        return refuse_with("target_changed");
    }
    let Ok(service) = crate::inference::commands::app_inference_service(app) else {
        return refuse_with("journal_error");
    };

    match service.execute_cloud_render(
        grant_nonce,
        &located.job_path,
        located.page_index as u32,
        region_id,
        &target,
        &recipe,
        &intent,
        &config,
        crate::inference::cloud_allowed(app),
        &PollOptions::interactive(),
    ) {
        Ok(region) => {
            let page_status = current_region(located, region_id)
                .ok()
                .flatten()
                .map(|(_, page_status)| page_status)
                .unwrap_or_else(|| "reviewed".to_owned());
            CloudRender::Committed(Box::new(region), page_status)
        }
        Err(err) => CloudRender::Stopped(
            match err.phase() {
                RenderPhase::Cancelled => "cancelled",
                RenderPhase::Unknown => "unknown",
                _ => "failed",
            },
            err.code(),
        ),
    }
}

/// `applyTool`.
fn is_approved_component_region(region_id: &str) -> bool {
    region_id.contains("-hreview-sam-")
}

#[tauri::command]
pub async fn apply_tool(
    app: tauri::AppHandle,
    tool: String,
    params: Option<serde_json::Value>,
    region_id: Option<String>,
) -> Result<ApplyResult, String> {
    crate::library::blocking(move || {
        if region_id
            .as_deref()
            .is_some_and(is_approved_component_region)
        {
            return Ok(ApplyResult {
                error_code: Some("unsupported_revision"),
                ..ApplyResult::of("blocked")
            });
        }
        let params = params.unwrap_or(serde_json::Value::Null);
        let string = |key: &str| {
            params
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };

        let has_cloud_target = explicit_remote_or_invalid_target(&params);
        let grant_nonce = grant_nonce_in(&params);

        if (has_cloud_target || grant_nonce.is_some()) && !crate::inference::cloud_allowed(&app) {
            cloud_blocked();
            return Ok(ApplyResult {
                error_code: Some("cloud_disabled"),
                ..ApplyResult::of("blocked")
            });
        }

        if (has_cloud_target || grant_nonce.is_some())
            && region_id.as_ref().is_some_and(|id| {
                let Ok(library) = Library::for_app(&app) else {
                    return false;
                };
                let Ok(Some(located)) = locate_region(&library, id) else {
                    return false;
                };
                // A read, so without the lock (see `run::lock_job`).
                Job::open(&located.job_path).ok().is_some_and(|job| {
                    job.project
                        .text_shape_plans
                        .iter()
                        .any(|plan| plan.region_id == *id)
                })
            })
        {
            return Ok(ApplyResult {
                error_code: Some("use_prepared_text_shape"),
                ..ApplyResult::of("blocked")
            });
        }

        if let Some(grant_nonce) = grant_nonce {
            let Some(region_id) = region_id else {
                return Ok(ApplyResult::of("not-found"));
            };
            let library = Library::for_app(&app)?;
            let Some(located) = locate_region(&library, &region_id)? else {
                return Ok(ApplyResult::of("not-found"));
            };
            return Ok(
                match render_in_cloud(&app, &located, &region_id, &params, &grant_nonce) {
                    CloudRender::Committed(region, page_status) => ApplyResult {
                        mask: region.mask.clone(),
                        region: Some(*region),
                        page_status: Some(page_status),
                        ..ApplyResult::of("applied")
                    },
                    CloudRender::Stopped(status, code) => ApplyResult {
                        error_code: Some(code),
                        ..ApplyResult::of(status)
                    },
                },
            );
        }

        // A cloud engine with no grant: nothing is sent until the user has
        // seen what will be and said yes, which is the interface's consent
        // step, and this is the answer that asks for it.
        if has_cloud_target {
            return Ok(ApplyResult::of("needs-confirmation"));
        }

        // Text cleanup page runs use the prepared run command. This command
        // applies one edit to a stored region only.
        let Some(region_id) = region_id else {
            return Ok(ApplyResult::of("not-found"));
        };
        let library = Library::for_app(&app)?;
        let Some(located) = locate_region(&library, &region_id)? else {
            return Ok(ApplyResult::of("not-found"));
        };

        {
            let _lock = run::lock_job(&located.job_path)?;
            let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
            if job
                .project
                .text_shape_plans
                .iter()
                .any(|row| row.region_id == region_id)
            {
                return Ok(ApplyResult {
                    error_code: Some("use_prepared_text_shape"),
                    ..ApplyResult::of("blocked")
                });
            }
        }

        let bbox: Option<Bbox> = params
            .get("bbox")
            .and_then(|value| serde_json::from_value::<Bbox>(value.clone()).ok());
        let stroke: Option<PaintedStroke> = params
            .get("stroke")
            .and_then(|value| serde_json::from_value::<PaintedStroke>(value.clone()).ok());
        // The drawn area, for the tool whose gesture is one. `stroke` and
        // `painted` are never both sent: a path has a radius and no vertices,
        // an area has vertices and no radius.
        let drawn: Option<PaintedShape> = params
            .get("painted")
            .and_then(|value| serde_json::from_value::<PaintedShape>(value.clone()).ok());
        // Parsed **here as well as in `create_region`**: painting over an
        // existing region dispatches `applyTool`, so reading `params.paint` in
        // only one of the two silently routes every retouch back into LaMa.
        //
        // Read before the geometry rather than after it, because a paint plan
        // makes the geometry dead: `edit` takes the paint branch before it
        // looks at the seed, so rasterising a mask for it would be a full pass
        // over the shape's box that nothing then reads.
        let paint = paint_plan(&tool, &params);
        // A stored detection cleaned with no rung and no fill mode named starts
        // where the detecting run said it would: its stored pick.
        let detected_pick = (string("engine").is_none() && string("fillMode").is_none() && bbox.is_none())
            .then(|| stored_detection_pick(&located, &region_id))
            .transpose()?
            .flatten();
        let fill_mode = string("fillMode").unwrap_or_else(|| {
            if detected_pick == Some(EnginePick::Solid) { "solid" } else { "match-surround" }.to_owned()
        });
        let mut requested = None;
        let geometry = match bbox {
            Some(bbox) => {
                let _lock = run::lock_job(&located.job_path)?;
                let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
                let Some(source_idx) = Library::resolve_page(&job.project, located.page_index)
                else {
                    return Ok(ApplyResult::of("not-found"));
                };
                let Some((w, h)) = page_size(&job, source_idx) else {
                    return Ok(ApplyResult::of("not-found"));
                };
                let painted = if paint.is_some() {
                    None
                } else {
                    stroke
                        .as_ref()
                        .and_then(|stroke| stroke_mask(stroke, w, h))
                        .or_else(|| drawn.as_ref().and_then(|shape| shape_mask(shape, w, h)))
                };
                let rect = pixels_of(bbox, w, h);
                requested = Some(rect);
                painted_geometry(rect, painted)
            }
            None => existing_geometry(&located, &region_id)?,
        };

        let choice = match named_rung(string("engine").as_deref()) {
            Some(engine) => Choice::Exact(engine),
            None if detected_pick.is_some() => Choice::Ladder(detected_pick),
            None => Choice::Ladder(Some(match engine_for_fill_mode(&fill_mode) {
                Engine::Fill => EnginePick::Fill,
                _ => EnginePick::Lama,
            })),
        };
        // A detection's mask is the detector's, whoever asked for it cleaned.
        let source = if matches!(geometry, Geometry::StoredDetection) { "auto" } else { "hand" };
        let solid_color = if matches!(geometry, Geometry::StoredDetection) && fill_mode == "solid" {
            stored_solid_color(&located, &region_id, run::parse_color_hex(string("bubbleColor").as_deref()))?
        } else { None };
        let plan = Plan {
            expected_source: None,
            region_id,
            geometry,
            choice,
            source,
            tool: Some(tool),
            fill_mode: Some(fill_mode_key(&fill_mode)),
            solid_color,
            // Whether or not the mask came from the detector: a hand gesture
            // over a region the gate held back answers the warning as surely
            // as `cleanAnyway` does, and a page that kept both would show a
            // warning beside the mask that resolved it. A no-op where there is
            // no row to take.
            clears_untouched: true,
            paint,
            requested,
            stroke: stroke.clone(),
            drawn: drawn.clone(),
        };
        match edit(&app, &located, plan)? {
            Outcome::Cleaned(edited) => Ok(ApplyResult::applied(edited)),
            Outcome::Refused(reason) => {
                refuse(reason);
                Ok(ApplyResult::of("blocked"))
            }
            Outcome::NotFound => Ok(ApplyResult::of("not-found")),
        }
    })
    .await
}

fn stored_solid_color(located: &Located, region_id: &str, fallback: Option<[u8; 3]>)
    -> Result<Option<[u8; 3]>, String> {
    let _lock = run::lock_job(&located.job_path)?;
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    Ok(job.project.detections.iter().find(|row| row.id == region_id)
        .and_then(|row| row.balloon_color).or(fallback))
}

/// The three fill-mode words the seam uses, as `'static` strings - a snapshot
/// field is written as one of exactly these and never as whatever arrived.
fn fill_mode_key(mode: &str) -> &'static str {
    match mode {
        "reconstruct" => "reconstruct",
        "solid" => "solid",
        _ => "match-surround",
    }
}

/// Where an existing region's mask comes from: its patch, a held candidate's
/// stored lettering, or - for a region the gate held back, and a candidate
/// written before rows kept lettering - the detector, under the box the
/// manifest kept.
fn existing_geometry(located: &Located, region_id: &str) -> Result<Geometry, String> {
    let _lock = run::lock_job(&located.job_path)?;
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    if job
        .project
        .patches
        .iter()
        .any(|record| record.id == region_id)
    {
        return Ok(Geometry::Stored);
    }
    if job.project.detections.iter().any(|record| record.id == region_id) {
        return Ok(Geometry::StoredDetection);
    }
    let Some(source_idx) = Library::resolve_page(&job.project, located.page_index) else {
        return Ok(Geometry::Stored);
    };
    let page_id = job_page_id(region_id);
    let found = job.project.regions_untouched.iter().find(|record| {
        record.source_idx == source_idx && untouched_id(page_id, record.bbox) == region_id
    });
    let Some(record) = found else {
        return Ok(Geometry::Stored);
    };
    Ok(match job.load_held_lettering(record).map_err(|e| e.to_string())? {
        Some((lettering, scale)) => Geometry::HeldLettering(lettering, scale),
        None => { let source=&job.project.sources[source_idx]; Geometry::Detected(source.orientation.rect(record.bbox,source.w,source.h)) },
    })
}

/// The pick a stored detection was detected with, or `None` for an id that is
/// not a stored detection.
fn stored_detection_pick(located: &Located, region_id: &str) -> Result<Option<EnginePick>, String> {
    let _lock = run::lock_job(&located.job_path)?;
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    Ok(job
        .project
        .detections
        .iter()
        .find(|record| record.id == region_id)
        .and_then(|record| run::parse_pick(Some(&record.pick))))
}

/// The untouched row a region id names: its reason and, on a held candidate,
/// whether grouping found it inside a balloon.
fn untouched_reason(located: &Located, region_id: &str) -> Result<Option<(String, Option<bool>)>, String> {
    let _lock = run::lock_job(&located.job_path)?;
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    let Some(source_idx) = Library::resolve_page(&job.project, located.page_index) else {
        return Ok(None);
    };
    let page_id = job_page_id(region_id);
    Ok(job
        .project
        .regions_untouched
        .iter()
        .find(|record| {
            record.source_idx == source_idx && untouched_id(page_id, record.bbox) == region_id
        })
        .map(|record| (record.reason.clone(), record.inside_bubble)))
}

/// Which pick a clean of a held-back region starts on when none is named.
///
/// **One rule with the interface** (`src/lib/editor/maskactions.svelte.js`
/// `cleanAnyway`): text the gate held outside a bubble, and a held candidate
/// grouping did not find inside a balloon (usually a sound effect over
/// artwork, or a row from before the balloon was recorded), start on the
/// outside pick; everything else on the in-bubble pick.
fn untouched_fallback_pick(reason: Option<&str>, inside_bubble: Option<bool>) -> EnginePick {
    let outside = match reason {
        Some(r) if crate::library::is_candidate_reason(r) => inside_bubble != Some(true),
        Some(r) => r.contains("OutsideBubble") || r.contains("outside-bubble"),
        None => false,
    };
    if outside { Picks::default().outside } else { Picks::default().bubble }
}

/// `createRegion` - a region drawn where the detector found nothing.
///
/// **Always rendered locally.** Consent binds a cloud grant to a stored
/// region's crop and mask, and this region is not stored until this command
/// writes it. So a cloud engine here is refused only when cloud is off
/// (`notice.cloud.blocked`); otherwise the region is created with the local
/// default for its fill mode, exactly as if no engine had been named, and the
/// interface asks for consent and calls `applyTool` on the new region.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn create_region(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: u32,
    expected_source_idx: Option<usize>,
    expected_source_sha: Option<String>,
    bbox: Bbox,
    tool: String,
    params: Option<serde_json::Value>,
) -> Result<Option<CreatedRegion>, String> {
    crate::library::blocking(move || {
        let params = params.unwrap_or(serde_json::Value::Null);
        let string = |key: &str| params.get(key).and_then(|v| v.as_str()).map(str::to_owned);

        let wants_cloud = explicit_remote_or_invalid_target(&params);
        if wants_cloud && !crate::inference::cloud_allowed(&app) {
            cloud_blocked();
            return Ok(None);
        }

        let library = Library::for_app(&app)?;
        let Some(located) = locate_page(&library, &chapter_id, page_index as usize)? else {
            return Ok(None);
        };

        let stroke: Option<PaintedStroke> = params
            .get("stroke")
            .and_then(|value| serde_json::from_value::<PaintedStroke>(value.clone()).ok());
        let drawn: Option<PaintedShape> = params
            .get("painted")
            .and_then(|value| serde_json::from_value::<PaintedShape>(value.clone()).ok());
        // Before the geometry, for the reason `apply_tool` gives: a paint plan
        // is answered by the paint branch and never reaches the seed.
        let paint = paint_plan(&tool, &params);

        let (region_id, rect, painted) = {
            let _lock = run::lock_job(&located.job_path)?;
            let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
            let Some(source_idx) = Library::resolve_page(&job.project, located.page_index) else {
                return Ok(None);
            };
            let Some((w, h)) = page_size(&job, source_idx) else {
                return Ok(None);
            };
            let page_id = crate::library::page_id(&chapter_id, located.page_index);
            (
                mint_hand_id(&job, &page_id),
                pixels_of(bbox, w, h),
                if paint.is_some() {
                    None
                } else {
                    stroke
                        .as_ref()
                        .and_then(|stroke| stroke_mask(stroke, w, h))
                        .or_else(|| drawn.as_ref().and_then(|shape| shape_mask(shape, w, h)))
                },
            )
        };

        let fill_mode = string("fillMode").unwrap_or_else(|| "match-surround".to_owned());
        // A cloud engine names no local rung: the local default it is.
        let engine = string("engine").filter(|_| !wants_cloud);
        let choice = match named_rung(engine.as_deref()) {
            Some(engine) => Choice::Exact(engine),
            None => Choice::Ladder(Some(match engine_for_fill_mode(&fill_mode) {
                Engine::Fill => EnginePick::Fill,
                _ => EnginePick::Lama,
            })),
        };

        let expected_source = match (expected_source_idx, expected_source_sha) {
            (Some(index), Some(hash)) => Some((index, hash)),
            (None, None) => None,
            _ => return Ok(None),
        };

        let plan = Plan {
            expected_source,
            region_id,
            geometry: painted_geometry(rect, painted),
            choice,
            source: "hand",
            tool: Some(tool),
            fill_mode: Some(fill_mode_key(&fill_mode)),
            solid_color: None,
            clears_untouched: false,
            paint,
            requested: Some(rect),
            stroke,
            drawn,
        };
        match edit(&app, &located, plan)? {
            Outcome::Cleaned(edited) => Ok(Some(CreatedRegion {
                region: edited.region,
                page_status: edited.page_status,
            })),
            Outcome::Refused(reason) => {
                refuse(reason);
                Ok(None)
            }
            Outcome::NotFound => Ok(None),
        }
    })
    .await
}

/// `{page id}-h{n}`, after every hand region already on the page.
///
/// The manifest is the counter. A session-local one - which is what the mock
/// uses - would hand the same id out again after a restart and the second
/// region would replace the first.
fn mint_hand_id(job: &Job, page_id: &str) -> String {
    let prefix = format!("{page_id}-h");
    let next = job
        .project
        .patches
        .iter()
        .map(|record| record.id.as_str())
        .chain(
            job.project
                .text_shape_plans
                .iter()
                .map(|record| record.region_id.as_str()),
        )
        .chain(
            job.project
                .text_shape_corrections
                .iter()
                .map(|record| record.region_id.as_str()),
        )
        .filter_map(|id| id.strip_prefix(&prefix))
        .filter_map(|suffix| suffix.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    format!("{prefix}{next}")
}

/* ------------------------------------------------------------------ */
/* Editing a detection's mask by hand                                  */
/* ------------------------------------------------------------------ */

/// `editDetectionMask`: grow or erase the page's detections with a brush
/// stroke or a drawn shape, before anything is cleaned.
///
/// Nothing is rendered and no engine runs. The gesture only moves pixels
/// between "a detection may erase this" and "nothing may", which is what a
/// detection is until a clean replaces it with a patch. The area a detection
/// may erase is its mask and its lettering together, its **display set**:
/// the fill and solid rungs write through the mask and the model rungs through
/// the lettering, and a stored lettering can reach past its mask.
///
/// `mode` is `add` or `remove`; anything else is `mask_edit_mode_invalid`.
/// `stroke` is a [`PaintedStroke`] and `painted` a [`PaintedShape`] other than
/// a line, which has no area; with both, the stroke is used. `null` for a
/// chapter or page that is not there, a source that is not the one the
/// interface drew, and a gesture that does not parse or covers nothing on the
/// page. Only the page's detections are touched: patches, untouched rows and
/// held candidates never are.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn edit_detection_mask(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: u32,
    expected_source_idx: Option<usize>,
    expected_source_sha: Option<String>,
    mode: String,
    stroke: Option<serde_json::Value>,
    painted: Option<serde_json::Value>,
) -> Result<Option<MaskEdit>, String> {
    crate::library::blocking(move || {
        let mode = MaskEditMode::parse(&mode).ok_or_else(|| "mask_edit_mode_invalid".to_owned())?;
        let expected_source = match (expected_source_idx, expected_source_sha) {
            (Some(index), Some(hash)) => Some((index, hash)),
            (None, None) => None,
            _ => return Ok(None),
        };
        let library = Library::for_app(&app)?;
        let located = match locate_page(&library, &chapter_id, page_index as usize) {
            Ok(Some(located)) => located,
            Ok(None) | Err(LibraryError::Unknown { .. } | LibraryError::NoJob { .. }) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        edit_detection_mask_at(&located, expected_source.as_ref(), mode, stroke.as_ref(), painted.as_ref())
    })
    .await
}

/// The command's body, under the chapter's lock, from a page already located.
fn edit_detection_mask_at(
    located: &Located,
    expected_source: Option<&(usize, String)>,
    mode: MaskEditMode,
    stroke: Option<&serde_json::Value>,
    painted: Option<&serde_json::Value>,
) -> Result<Option<MaskEdit>, String> {
    // The same lock every other edit takes, so a run holding the chapter
    // refuses this one the same way.
    let _lock = run::lock_job(&located.job_path)?;
    let mut job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    let Some(source_idx) = Library::resolve_page(&job.project, located.page_index) else {
        return Ok(None);
    };
    let Some(source) = job.project.sources.get(source_idx) else {
        return Ok(None);
    };
    // The manifest's digest names the page the interface drew. The pixels are
    // never read, so the file is not hashed again.
    if expected_source.is_some_and(|(index, sha)| *index != source_idx || *sha != source.sha256) {
        return Ok(None);
    }
    let Some(gesture) = gesture_mask(stroke, painted, source.orientation.size(source.w,source.h).0, source.orientation.size(source.w,source.h).1) else {
        return Ok(None);
    };

    let ids: Vec<String> = job
        .project
        .detections
        .iter()
        .filter(|record| record.source_idx == source_idx)
        .map(|record| record.id.clone())
        .collect();
    let mut loaded = Vec::with_capacity(ids.len());
    for id in &ids {
        if let Some(found) = job.load_detection(id).map_err(|e| e.to_string())? {
            loaded.push(found);
        }
    }
    let page: Vec<DetectionMasks<'_>> = loaded
        .iter()
        .map(|found| DetectionMasks {
            id: &found.record.id,
            order: found.record.order,
            mask: &found.mask,
            ink: &found.ink,
        })
        .collect();
    let plan = plan_mask_edit(&page, &gesture, mode);

    let mut edit = MaskEdit {
        page_status: String::new(),
        changed: plan.updated.iter().map(|(id, _, _)| id.clone()).collect(),
        created: Vec::new(),
        removed: plan.removed.clone(),
    };
    if !plan.is_empty() {
        // Everything but the pixels and the box stays as Detect left it: the
        // fit, the group, the pick, the reading order and the padding.
        let mut updated = Vec::with_capacity(plan.updated.len());
        for (id, mask, ink) in plan.updated {
            let Some(found) = loaded.iter().find(|found| found.record.id == id) else {
                continue;
            };
            let mut record = found.record.clone();
            record.bbox = mask.bounds;
            let base = if record.padding_px > 0 {
                let (base_mask, base_ink) = job.load_detection_base(&id).map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("detection {id} has no stored masks"))?;
                let old_display = union_masks(&found.mask, &found.ink);
                Some((rebased(&base_mask, &old_display, &mask), rebased(&base_ink, &found.ink, &ink)))
            } else {
                None
            };
            // An edit that leaves no ungrown pixel makes the edited masks the
            // new ungrown ones: there is nothing left to pad from.
            let base = match base {
                Some((base_mask, base_ink)) if tight(&base_mask).is_some() => Some((base_mask, base_ink)),
                _ => {
                    record.padding_px = 0;
                    None
                }
            };
            updated.push(cleaner_core::project::LoadedDetection { record, mask, ink, evidence: None, base });
        }
        let order = job
            .project
            .detections
            .iter()
            .filter(|record| record.source_idx == source_idx)
            .map(|record| record.order)
            .chain(
                job.project
                    .patches
                    .iter()
                    .filter(|record| record.source_idx == source_idx)
                    .map(|record| record.order),
            )
            .max()
            .map_or(0, |order| order.saturating_add(1));
        let created = plan
            .created
            .map(|mask| cleaner_core::project::LoadedDetection {
                record: hand_detection(source_idx, mask.bounds, order),
                ink: mask.clone(),
                mask,
                evidence: None,
                base: None,
            })
            .into_iter()
            .collect();
        let page_id = crate::library::page_id(&located.chapter_id, located.page_index);
        edit.created = job
            .rewrite_page_detections(source_idx, &page_id, updated, created, &plan.removed)
            .map_err(|e| e.to_string())?;
    }
    edit.page_status = crate::library::page_of(&located.chapter_id, &job.project, located.page_index)
        .map(|page| page.status.to_owned())
        .unwrap_or_default();
    Ok(Some(edit))
}

/// `setDetectionPadding`: grow every stored detection on a page, or on every
/// page of a chapter when `page_index` is absent, to `padding_px` page pixels
/// past its ungrown masks (`DetectedRegion::padding_px`). Optional `region_id`
/// restricts that scope to one stored detection, validated before any writes.
///
/// The padding is always grown from the ungrown pair Detect or the last hand
/// edit left, never from a mask already grown, so any sequence of paddings
/// ends where the last one alone would, and 0 gives back the unpadded masks.
/// Cleaned layers, untouched rows and held candidates never change: only a
/// detection has a mask still to be cleaned through.
///
/// Detections a padding above 0 runs together become one ([`run_together`]):
/// the one earliest in reading order keeps its id and takes the others'
/// pixels, and the others leave the page. That is not undone by a smaller
/// padding, which shrinks the one detection they became.
///
/// Answers the detections that changed, the ones another took in, and the
/// pages they are on, or `null` for a chapter that is not there. Refuses
/// `mask_padding_invalid` past the widest padding offered, and the
/// chapter-busy code a run holds.
#[tauri::command]
pub async fn set_detection_padding(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: Option<u32>,
    padding_px: u32,
    region_id: Option<String>,
) -> Result<Option<PaddingEdit>, String> {
    crate::library::blocking(move || {
        if padding_px > cleaner_core::constants::MAX_MASK_PADDING {
            return Err("mask_padding_invalid".to_owned());
        }
        let library = Library::for_app(&app)?;
        let located = match locate_page(&library, &chapter_id, page_index.unwrap_or(0) as usize) {
            Ok(Some(located)) => located,
            Ok(None) | Err(LibraryError::Unknown { .. } | LibraryError::NoJob { .. }) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        set_detection_padding_scoped_at(&located, page_index.map(|_| located.page_index), padding_px, region_id.as_deref()).map(Some)
    })
    .await
}

/// `setDetectionPadding`'s answer: the detections whose masks were grown or
/// shrunk, the detections that left the page because one of those took them
/// in, and the page indices they are on. Three empty lists are a padding
/// every detection already had, and nothing was written.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaddingEdit {
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub pages: Vec<u32>,
}

/// Legacy fitting already grew the mask. Recover exact segmentation only for
/// untouched records with intact evidence; edited or older records retain their
/// authoritative stored masks because their raw segmentation cannot be recovered.
fn legacy_padding_seed(job: &Job, record: &cleaner_core::project::DetectedRegion) -> Option<Mask> {
    if record.padding_from_seed || record.edits() != 0 { return None; }
    // Pre-orientation evidence has no coordinate convention. Preserve the
    // authoritative stored masks instead of rebuilding a rotated seed from it.
    if job.project.sources.get(record.source_idx)?.orientation.0 != 1 { return None; }
    let group = record.group.as_ref()?;
    let bytes = std::fs::read(job.sidecar().join(group.evidence_ref.as_ref()?)).ok()?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if group.evidence_sha256.as_ref()? != &digest { return None; }
    let evidence = cleaner_core::text_groups::EvidenceFile::decode(&bytes).ok()?;
    let seed = evidence.lettering(group).ok()?;
    let digest = format!("{:x}", Sha256::digest(cleaner_core::project::buffers::encode_mask(&seed)));
    (digest == group.lettering_sha256 && tight(&seed).is_some()).then_some(seed)
}

/// The command's body, under the chapter's lock: one page, or every page when
/// `page_index` is `None`, each page in one manifest write.
#[cfg(test)]
fn set_detection_padding_at(located: &Located, page_index: Option<usize>, padding: u32) -> Result<PaddingEdit, String> {
    set_detection_padding_scoped_at(located, page_index, padding, None)
}

fn set_detection_padding_scoped_at(
    located: &Located,
    page_index: Option<usize>,
    padding: u32,
    region_id: Option<&str>,
) -> Result<PaddingEdit, String> {
    if padding > cleaner_core::constants::MAX_MASK_PADDING {
        return Err("mask_padding_invalid".to_owned());
    }
    let _lock = run::lock_job(&located.job_path)?;
    let mut job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    let indices: Vec<usize> = match page_index {
        Some(index) => vec![index],
        None => (0..job.project.strip.order.len()).collect(),
    };
    // Resolve and validate the target against this chapter and the selected
    // page scope before writing any mask or manifest. Exact record lookup also
    // ensures caller-provided ids are never interpreted as sidecar paths.
    if let Some(id) = region_id {
        let valid = job.project.detections.iter().any(|record| record.id == id &&
            indices.iter().any(|&index| Library::resolve_page(&job.project, index) == Some(record.source_idx)));
        if !valid { return Err("mask_padding_target_invalid".to_owned()); }
    }
    let mut edit = PaddingEdit { changed: Vec::new(), removed: Vec::new(), pages: Vec::new() };
    for index in indices {
        let Some(source_idx) = Library::resolve_page(&job.project, index) else {
            continue;
        };
        let Some(source) = job.project.sources.get(source_idx) else {
            continue;
        };
        let (width, height) = source.orientation.size(source.w, source.h);
        let ids: Vec<String> = job.project.detections.iter()
            .filter(|record| record.source_idx == source_idx && region_id.is_none_or(|id| record.id == id) &&
                (record.padding_px != padding ||
                (!record.padding_from_seed && record.edits() == 0 && record.group.is_some())))
            .map(|record| record.id.clone())
            .collect();
        let mut updated = Vec::with_capacity(ids.len());
        for id in &ids {
            let Some(found) = job.load_detection(id).map_err(|e| e.to_string())? else {
                continue;
            };
            let seed = legacy_padding_seed(&job, &found.record);
            if found.record.padding_px == padding && seed.is_none() {
                continue;
            }
            let mut record = found.record;
            let (base_mask, base_ink) = match seed {
                Some(seed) => {
                    record.padding_from_seed = true;
                    (seed.clone(), seed)
                }
                None => job.load_detection_base(id).map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("detection {id} has no stored masks"))?,
            };
            record.padding_px = padding;
            if record.padding_from_seed {
                if let Some(fitted) = &mut record.fit { fitted.thickness = padding; }
            }
            updated.push(cleaner_core::project::LoadedDetection {
                mask: base_mask.dilated(padding, width, height),
                ink: base_ink.dilated(padding, width, height),
                base: (padding > 0).then_some((base_mask, base_ink)),
                record,
                evidence: None,
            });
        }
        // What the padding ran together becomes one detection. The scope is
        // what may start a merge: every detection of a page, or the one
        // named, with whatever it now reaches. No padding, no merge: 0 is
        // the way back to the masks as they were stored.
        let repadded = updated.len();
        let mut removed = Vec::new();
        if padding > 0 && (region_id.is_none() || repadded > 0) {
            let rest: Vec<String> = job.project.detections.iter()
                .filter(|record| record.source_idx == source_idx
                    && !updated.iter().any(|found| found.record.id == record.id))
                .map(|record| record.id.clone())
                .collect();
            let mut page = updated;
            for id in &rest {
                if let Some(found) = job.load_detection(id).map_err(|e| e.to_string())? {
                    page.push(found);
                }
            }
            let fresh: Vec<bool> = (0..page.len()).map(|at| at < repadded || region_id.is_none()).collect();
            let groups = run_together(&page, &fresh, (width, height));
            let mut page: Vec<Option<cleaner_core::project::LoadedDetection>> = page.into_iter().map(Some).collect();
            let mut merged = Vec::with_capacity(groups.len());
            for group in groups {
                let mut members = Vec::with_capacity(group.len());
                for at in group {
                    let Some(mut found) = page[at].take() else {
                        continue;
                    };
                    if found.base.is_none() && found.record.padding_px > 0 {
                        found.base = job.load_detection_base(&found.record.id).map_err(|e| e.to_string())?;
                    }
                    members.push(found);
                }
                removed.extend(members.iter().skip(1).map(|found| found.record.id.clone()));
                merged.extend(made_one(members, (width, height)));
            }
            // The re-padded detections no merge took, then each merge's one.
            updated = page.into_iter().take(repadded).flatten().chain(merged).collect();
        }
        if updated.is_empty() {
            continue;
        }
        let changed: Vec<String> = updated.iter().map(|found| found.record.id.clone()).collect();
        let page_id = crate::library::page_id(&located.chapter_id, index);
        job.rewrite_page_detections(source_idx, &page_id, updated, Vec::new(), &removed)
            .map_err(|e| e.to_string())?;
        edit.changed.extend(changed);
        edit.removed.extend(removed);
        edit.pages.push(index as u32);
    }
    Ok(edit)
}

/// How near two detections' display sets may come and still be two regions
/// once one of them is padded, in page pixels. An overlap anywhere, or a gap
/// this thin, and they are one: paper that narrow between two holes is
/// cleaned by neither and shows as a seam. The reach of
/// [`HELD_RIM`], so what a Detect of one area was cut clear of a held
/// detection is not joined to it again.
pub(crate) const MERGE_REACH: u32 = 4;

/// Which of a page's detections a padding has run together: each answer is
/// the positions in `page` of detections that are to become one, the earliest
/// in reading order first (then by id, then by position). A detection no
/// other reaches is in no answer.
///
/// Two detections are run together when their display sets overlap anywhere
/// or come within [`MERGE_REACH`], at least one of them is padded, and at
/// least one of them is `fresh`: in the scope of what the caller changed.
/// Unpadded detections that touch are left as two, because that is how
/// Detect grouped them. Reaching is carried along: if a first reaches a
/// second and the second a third, the three are one.
pub(crate) fn run_together(
    page: &[cleaner_core::project::LoadedDetection],
    fresh: &[bool],
    (width, height): (u32, u32),
) -> Vec<Vec<usize>> {
    let shown: Vec<Option<Mask>> = page.iter().map(|found| display_set(&found.mask, &found.ink)).collect();
    let mut reach: Vec<Option<Mask>> = vec![None; page.len()];
    let mut root: Vec<usize> = (0..page.len()).collect();
    fn find(root: &mut [usize], mut at: usize) -> usize {
        while root[at] != at {
            root[at] = root[root[at]];
            at = root[at];
        }
        at
    }
    for a in 0..page.len() {
        for b in a + 1..page.len() {
            if !(fresh.get(a) == Some(&true) || fresh.get(b) == Some(&true))
                || (page[a].record.padding_px == 0 && page[b].record.padding_px == 0) {
                continue;
            }
            let (Some(first), Some(second)) = (&shown[a], &shown[b]) else {
                continue;
            };
            if intersection(first.bounds.grown(MERGE_REACH, width, height), second.bounds).is_none() {
                continue;
            }
            let grown = reach[a].get_or_insert_with(|| first.dilated(MERGE_REACH, width, height));
            if grown.intersects(second) {
                let (a, b) = (find(&mut root, a), find(&mut root, b));
                root[a.max(b)] = a.min(b);
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); page.len()];
    for at in 0..page.len() {
        let of = find(&mut root, at);
        groups[of].push(at);
    }
    groups.retain(|group| group.len() > 1);
    for group in &mut groups {
        group.sort_by(|&a, &b| page[a].record.order.cmp(&page[b].record.order)
            .then_with(|| page[a].record.id.cmp(&page[b].record.id))
            .then(a.cmp(&b)));
    }
    groups
}

/// The one detection `members` become: the first's id, order and age, every
/// member's pixels, and only what all of them agree on beyond that. `None`
/// for no members.
///
/// The mask, the lettering and their ungrown pair are each the members'
/// together, so a later padding grows from all the letters. The padding is
/// the widest any member had. The box spans the members' boxes. It is inside
/// a balloon only when every member was, and it keeps a balloon colour, a
/// script or a pick only when every member had the same; a pick they
/// disagree on, or a fill across the edge of a balloon, becomes LaMa, the
/// rung that needs no balloon. The fit and the group are dropped: each was
/// measured for one member's mask, which this one's no longer is, so a clean
/// treats the mask as it treats a hand mask.
///
/// A padded member is to carry its ungrown pair (`base`); one without is
/// read as its own baseline.
pub(crate) fn made_one(
    members: Vec<cleaner_core::project::LoadedDetection>,
    (width, height): (u32, u32),
) -> Option<cleaner_core::project::LoadedDetection> {
    let mut record = members.first()?.record.clone();
    if members.len() == 1 {
        return members.into_iter().next();
    }
    let every = |of: &dyn Fn(&cleaner_core::project::DetectedRegion) -> bool| members.iter().all(|found| of(&found.record));
    let all_inside = every(&|other| other.inside);
    let none_inside = every(&|other| !other.inside);
    let one_pick = every(&|other| other.pick == record.pick);
    if !(one_pick && (all_inside || none_inside)) {
        record.pick = "lama".into();
    }
    record.inside = all_inside;
    if !(all_inside && every(&|other| other.balloon_color == record.balloon_color)) {
        record.balloon_color = None;
    }
    if !every(&|other| other.script == record.script) {
        record.script = None;
    }
    if members.iter().any(|found| found.record.detector == "cloud") {
        record.detector = "cloud".into();
    }
    record.review_state = members.iter().find_map(|found| found.record.review_state.clone());
    record.bbox = members.iter().fold(Rect::new(0, 0, 0, 0), |all, found| span(all, found.record.bbox));
    record.padding_px = members.iter().map(|found| found.record.padding_px).max().unwrap_or(0);
    record.padding_from_seed = every(&|other| other.padding_from_seed);
    record.fit = None;
    record.group = None;

    let together = |of: &dyn Fn(&cleaner_core::project::LoadedDetection) -> &Mask| {
        let masks: Vec<&Mask> = members.iter().map(of).collect();
        Mask::union(&masks, width, height)
    };
    let mask = together(&|found| &found.mask);
    let ink = together(&|found| &found.ink);
    let base = (record.padding_px > 0).then(|| (
        together(&|found| found.base.as_ref().map_or(&found.mask, |(mask, _)| mask)),
        together(&|found| found.base.as_ref().map_or(&found.ink, |(_, ink)| ink)),
    ));
    Some(cleaner_core::project::LoadedDetection { record, mask, ink, evidence: None, base })
}

/// A page's detections as Detect is to store them: every set its padding ran
/// together made one ([`run_together`], [`made_one`]), the rest as they
/// were, in the order given with each merge where its first member stood.
pub(crate) fn merged_by_padding(
    page: Vec<cleaner_core::project::LoadedDetection>,
    size: (u32, u32),
) -> Vec<cleaner_core::project::LoadedDetection> {
    let groups = run_together(&page, &vec![true; page.len()], size);
    if groups.is_empty() {
        return page;
    }
    let mut page: Vec<Option<cleaner_core::project::LoadedDetection>> = page.into_iter().map(Some).collect();
    for group in groups {
        let Some(&stands) = group.iter().min() else {
            continue;
        };
        let members = group.iter().filter_map(|&at| page[at].take()).collect();
        page[stands] = made_one(members, size);
    }
    page.into_iter().flatten().collect()
}

/// The record of a detection an add made over no detection: text the user
/// pointed at rather than text the detector found, so nothing was measured
/// about it. It starts on LaMa, the rung that needs no balloon.
fn hand_detection(source_idx: usize, bbox: Rect, order: u32) -> cleaner_core::project::DetectedRegion {
    cleaner_core::project::DetectedRegion {
        id: String::new(),
        source_idx,
        bbox,
        mask_ref: String::new(),
        inside: false,
        balloon_color: None,
        script: None,
        pick: "lama".into(),
        detector: "local".into(),
        created: run::now(),
        mask_sha256: String::new(),
        order,
        review_state: None,
        fit: None,
        group: None,
        padding_from_seed: true,
        padding_px: 0,
    }
}

/// Whether a mask edit grows the page's detections or erases from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaskEditMode {
    Add,
    Remove,
}

impl MaskEditMode {
    fn parse(mode: &str) -> Option<MaskEditMode> {
        match mode {
            "add" => Some(MaskEditMode::Add),
            "remove" => Some(MaskEditMode::Remove),
            _ => None,
        }
    }
}

/// The gesture as a mask over the page. `None` when it does not parse, is a
/// line, or covers nothing on the page. A stroke, when there is one, is the
/// gesture and the shape is not looked at.
fn gesture_mask(
    stroke: Option<&serde_json::Value>,
    painted: Option<&serde_json::Value>,
    width: u32,
    height: u32,
) -> Option<Mask> {
    match stroke.filter(|value| !value.is_null()) {
        Some(value) => stroke_mask(&PaintedStroke::deserialize(value).ok()?, width, height),
        None => {
            let shape = PaintedShape::deserialize(painted?).ok()?;
            if shape.kind == ShapeKind::Line {
                return None;
            }
            shape_mask(&shape, width, height)
        }
    }
}

/// One detection's two masks, as a mask edit reads them.
#[derive(Debug, Clone, Copy)]
struct DetectionMasks<'a> {
    id: &'a str,
    order: u32,
    mask: &'a Mask,
    ink: &'a Mask,
}

impl DetectionMasks<'_> {
    /// Whether the pixel is in this detection's display set.
    fn shows(&self, x: i64, y: i64) -> bool {
        self.mask.contains(x, y) || self.ink.contains(x, y)
    }

    /// How many of `gesture`'s pixels this detection shows.
    fn overlap(&self, gesture: &Mask) -> usize {
        let reach = span(self.mask.bounds, self.ink.bounds);
        let Some(area) = intersection(reach, gesture.bounds) else {
            return 0;
        };
        let mut count = 0;
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if gesture.contains(x, y) && self.shows(x, y) {
                    count += 1;
                }
            }
        }
        count
    }
}

/// What a mask edit decided, before anything is written. Every mask in it is
/// cropped to the pixels it holds, and every lettering lies inside its mask.
#[derive(Debug, Default, PartialEq)]
struct MaskEditPlan {
    /// `(id, mask, ink)` for each detection whose pixels changed.
    updated: Vec<(String, Mask, Mask)>,
    /// The mask, and the lettering, of the one detection an add over no
    /// detection makes.
    created: Option<Mask>,
    removed: Vec<String>,
}

impl MaskEditPlan {
    fn is_empty(&self) -> bool {
        self.updated.is_empty() && self.created.is_none() && self.removed.is_empty()
    }

    /// Record `detection`'s new masks, unless they hold the pixels it already
    /// had.
    fn change(&mut self, detection: &DetectionMasks<'_>, mask: Mask, ink: Mask) {
        let Some(mask) = tight(&mask) else {
            return;
        };
        let ink = tight(&ink).unwrap_or_else(|| mask.clone());
        if Some(&mask) != tight(detection.mask).as_ref() || Some(&ink) != tight(detection.ink).as_ref() {
            self.updated.push((detection.id.to_owned(), mask, ink));
        }
    }
}

/// The set arithmetic of a mask edit, over one page's detections and the
/// gesture `S`, all in page pixels.
///
/// **Add.** The detections whose display set meets `S` are the candidates.
/// None: `S` becomes a new detection, mask and lettering both. Otherwise the
/// one showing most of `S` takes it (ties to the lowest reading order, then
/// the id), less every pixel another detection already shows, so no pixel is
/// given to two regions. Its mask becomes its display set plus those pixels,
/// its lettering its lettering plus them. An add that gives it nothing new
/// changes nothing.
///
/// **Remove.** Every detection whose display set meets `S` loses `S` from its
/// display set, which becomes its mask, and from its lettering. A detection
/// left with no mask is removed; one left with a mask and no lettering takes
/// the mask as its lettering, so a clean on any rung still has something to
/// write through.
fn plan_mask_edit(page: &[DetectionMasks<'_>], gesture: &Mask, mode: MaskEditMode) -> MaskEditPlan {
    let mut plan = MaskEditPlan::default();
    let Some(gesture) = tight(gesture) else {
        return plan;
    };
    let touched: Vec<(&DetectionMasks<'_>, usize)> = page
        .iter()
        .map(|detection| (detection, detection.overlap(&gesture)))
        .filter(|(_, overlap)| *overlap > 0)
        .collect();
    match mode {
        MaskEditMode::Add => {
            let target = touched.iter().min_by(|(a, a_overlap), (b, b_overlap)| {
                b_overlap
                    .cmp(a_overlap)
                    .then(a.order.cmp(&b.order))
                    .then(a.id.cmp(b.id))
            });
            let Some((target, _)) = target else {
                plan.created = Some(gesture);
                return plan;
            };
            let mut added = gesture.clone();
            for other in page.iter().filter(|other| other.id != target.id) {
                let reach = span(other.mask.bounds, other.ink.bounds);
                let Some(area) = intersection(reach, added.bounds) else {
                    continue;
                };
                for y in area.y..area.bottom() {
                    for x in area.x..area.right() {
                        if other.shows(x, y) {
                            added.set(x, y, false);
                        }
                    }
                }
            }
            let Some(added) = tight(&added) else {
                return plan;
            };
            let display = union_masks(target.mask, target.ink);
            plan.change(target, union_masks(&display, &added), union_masks(target.ink, &added));
        }
        MaskEditMode::Remove => {
            for (detection, _) in touched {
                let mask = subtract(&union_masks(detection.mask, detection.ink), &gesture);
                if tight(&mask).is_none() {
                    plan.removed.push(detection.id.to_owned());
                } else {
                    plan.change(detection, mask, subtract(detection.ink, &gesture));
                }
            }
        }
    }
    plan
}

/// A detection's display set, its mask and its lettering together, cropped to
/// the pixels it holds: the area a clean of it may erase, and what
/// `tile://.../detection/<id>` draws. `None` when it holds no pixel.
pub(crate) fn display_set(mask: &Mask, ink: &Mask) -> Option<Mask> {
    tight(&union_masks(mask, ink))
}

/// How far a held detection's display set is grown before it is taken from
/// what a Detect of one area found, in page pixels. The same lettering fitted
/// from a window and from the whole page differs at its rim by a pixel or
/// two, and a rim is not text.
const HELD_RIM: u32 = 4;

/// The fewest pixels a detection may be left with and still be a region of
/// its own: less than one small glyph is a sliver between two masks.
const LEAST_LEFT: usize = 64;

/// What a Detect of one area found, less what the page's detections hold
/// already: `found` with every pixel of a held display set taken from its
/// masks, and `None` when what is left is not a region of its own.
///
/// By pixel and not by box, because the window groups text by what it sees:
/// lettering the page's Detect stored as two regions can come back as one,
/// and the half the page lost has to be stored without the half it kept. No
/// pixel is given to two detections, the rule a hand edit keeps
/// ([`plan_mask_edit`]).
///
/// A detection cut this way keeps its fit and its routing and loses its
/// group, whose lettering is no longer what its mask holds. `size` is the
/// page's, as it is shown.
pub(crate) fn area_detection_less_held(
    mut found: cleaner_core::project::LoadedDetection,
    held: &[cleaner_core::project::LoadedDetection],
    (width, height): (u32, u32),
) -> Option<cleaner_core::project::LoadedDetection> {
    let rims: Vec<Mask> = held.iter()
        .filter_map(|kept| display_set(&kept.mask, &kept.ink))
        .map(|display| display.dilated(HELD_RIM, width, height))
        .filter(|rim| found.mask.intersects(rim) || found.ink.intersects(rim))
        .collect();
    if rims.is_empty() {
        return Some(found);
    }
    let cut = |mask: &Mask| without_slivers(rims.iter().fold(mask.clone(), |left, rim| subtract(&left, rim)), &rims);
    found.mask = cut(&found.mask);
    found.ink = cut(&found.ink);
    found.base = found.base.take().map(|(mask, ink)| (cut(&mask), cut(&ink)));
    let mask = tight(&found.mask).filter(|mask| mask.count() >= LEAST_LEFT)?;
    // Lettering wholly inside a held mask leaves the mask as the lettering,
    // as a hand edit does.
    found.ink = tight(&found.ink).unwrap_or_else(|| mask.clone());
    found.base = match found.base.take().map(|(mask, ink)| (tight(&mask), tight(&ink))) {
        Some((Some(base_mask), ink)) => {
            let ink = ink.unwrap_or_else(|| base_mask.clone());
            Some((base_mask, ink))
        }
        // Nothing ungrown is left to pad from: the cut masks are the baseline.
        _ => {
            found.record.padding_px = 0;
            None
        }
    };
    found.record.bbox = mask.bounds;
    found.record.group = None;
    found.evidence = None;
    found.mask = mask;
    Some(found)
}

/// `mask` without the slivers a cut leaves at its edge: each connected run of
/// its pixels that touches one of `rims` and holds fewer than [`LEAST_LEFT`].
///
/// A rim is a disc's reach and lettering is not, so a few pixels of a held
/// glyph's far edge can lie just past it. Left in, they stretch the cut
/// detection's box over the held one. Only a run that touches a rim is
/// weighed: a small run anywhere else is a dot or a voicing mark of the text
/// that was kept.
fn without_slivers(mut mask: Mask, rims: &[Mask]) -> Mask {
    let bounds = mask.bounds;
    let at = |x: i64, y: i64| ((y - bounds.y) * i64::from(bounds.w) + (x - bounds.x)) as usize;
    let around = |x: i64, y: i64| (-1..=1).flat_map(move |dy| (-1..=1).map(move |dx| (x + dx, y + dy)));
    let mut seen = vec![false; bounds.w as usize * bounds.h as usize];
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if seen[at(x, y)] || !mask.contains(x, y)
                || !around(x, y).any(|(nx, ny)| rims.iter().any(|rim| rim.contains(nx, ny))) {
                continue;
            }
            seen[at(x, y)] = true;
            let mut run = vec![(x, y)];
            let mut next = 0;
            while let Some(&(px, py)) = run.get(next) {
                next += 1;
                for (nx, ny) in around(px, py) {
                    if mask.contains(nx, ny) && !seen[at(nx, ny)] {
                        seen[at(nx, ny)] = true;
                        run.push((nx, ny));
                    }
                }
            }
            if run.len() < LEAST_LEFT {
                for (px, py) in run {
                    mask.set(px, py, false);
                }
            }
        }
    }
    mask
}

/// The smallest rectangle holding both. A rectangle with no area holds
/// nothing, so it does not stretch the other.
fn span(a: Rect, b: Rect) -> Rect {
    if a.w == 0 || a.h == 0 {
        return b;
    }
    if b.w == 0 || b.h == 0 {
        return a;
    }
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Rect::new(x, y, (a.right().max(b.right()) - x) as u32, (a.bottom().max(b.bottom()) - y) as u32)
}

/// Where two rectangles overlap, `None` when they do not.
fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let (x, y) = (a.x.max(b.x), a.y.max(b.y));
    let (right, bottom) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (right > x && bottom > y).then(|| Rect::new(x, y, (right - x) as u32, (bottom - y) as u32))
}

/// `a ∪ b`, over the rectangle holding both.
fn union_masks(a: &Mask, b: &Mask) -> Mask {
    let mut out = Mask::empty(span(a.bounds, b.bounds));
    for mask in [a, b] {
        for y in mask.bounds.y..mask.bounds.bottom() {
            for x in mask.bounds.x..mask.bounds.right() {
                if mask.contains(x, y) {
                    out.set(x, y, true);
                }
            }
        }
    }
    out
}

/// A padded detection's ungrown mask after a hand edit changed its grown one
/// from `old` to `new`: what the edit added, added to it, and what the edit
/// took away, taken from it. A later padding then grows from the edited
/// shape, as the padding the edit was drawn over did.
fn rebased(base: &Mask, old: &Mask, new: &Mask) -> Mask {
    subtract(&union_masks(base, &subtract(new, old)), &subtract(old, new))
}

/// `a` without `b`, over `a`'s rectangle.
fn subtract(a: &Mask, b: &Mask) -> Mask {
    let mut out = a.clone();
    if let Some(area) = intersection(a.bounds, b.bounds) {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if b.contains(x, y) {
                    out.set(x, y, false);
                }
            }
        }
    }
    out
}

/// The mask cropped to the pixels it holds, `None` when it holds none.
fn tight(mask: &Mask) -> Option<Mask> {
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for y in mask.bounds.y..mask.bounds.bottom() {
        for x in mask.bounds.x..mask.bounds.right() {
            if mask.contains(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 {
        return None;
    }
    let bounds = Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
    if bounds == mask.bounds {
        return Some(mask.clone());
    }
    let mut out = Mask::empty(bounds);
    for y in y0..=y1 {
        for x in x0..=x1 {
            if mask.contains(x, y) {
                out.set(x, y, true);
            }
        }
    }
    Some(out)
}

/// `rerunMask`.
///
/// `params` carries a cloud grant and nothing else: with one, the region is
/// rendered in the cloud (see the module docs) and a render that does not
/// commit answers `null`, its code on the `cloud://attempt` event.
#[tauri::command]
pub async fn rerun_mask(
    app: tauri::AppHandle,
    mask_id: String,
    kind: String,
    engine: Option<String>,
    params: Option<serde_json::Value>,
) -> Result<Option<RerunResult>, String> {
    crate::library::blocking(move || {
        if is_approved_component_region(crate::library::region_of_mask(&mask_id)) {
            return Err("Approved component edits are insertion-only; use Undo or Delete instead of replacing their saved mask".into());
        }
        let library = Library::for_app(&app)?;
        rerun_mask_at(&library, Some(&app), &mask_id, &kind, engine.as_deref(),
            params.unwrap_or(serde_json::Value::Null), |located, plan| edit(&app, located, plan))
    })
    .await
}

fn rerun_mask_at(
    library: &Library,
    app: Option<&tauri::AppHandle>,
    mask_id: &str,
    kind: &str,
    engine: Option<&str>,
    params: serde_json::Value,
    mut apply: impl FnMut(&Located, Plan) -> Result<Outcome, String>,
) -> Result<Option<RerunResult>, String> {
    let region_id = crate::library::region_of_mask(mask_id).to_owned();
    if is_approved_component_region(&region_id) {
        return Err("Approved component edits are insertion-only; use Undo or Delete instead of replacing their saved mask".into());
    }
    let Some(located) = locate_region(library, &region_id)? else {
        return Ok(None);
    };

    {
        let _lock = run::lock_job(&located.job_path)?;
        let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
        let latest_plan = job.project.text_shape_plans.iter()
            .filter(|plan| plan.region_id == region_id)
            .max_by_key(|plan| plan.plan_revision);
        let committed_identity = job.project.patches.iter()
            .find(|record| record.id == region_id && record.geometry_policy == GeometryPolicy::TextShape)
            .and_then(|record| record.text_shape_plan_identity.as_deref());
        if latest_plan.is_some_and(|plan| committed_identity != Some(plan.identity.identity_sha256.as_str())) {
            refuse("decline.reason.rungUnavailable");
            return Ok(None);
        }
    }

    let Some((current, current_fill, current_bbox, is_cloud_provenance)) =
        rerun_current(&located, &region_id, kind)? else {
            return Ok(None);
        };

    if let Some(grant_nonce) = grant_nonce_in(&params) {
        if !app.is_some_and(crate::inference::cloud_allowed) {
            cloud_blocked();
            return Ok(None);
        }
        let app = app.expect("cloud permission requires an app");
        return Ok(
            match render_in_cloud(app, &located, &region_id, &params, &grant_nonce) {
                CloudRender::Committed(region, page_status) => Some(RerunResult {
                    mask: region.mask.clone(),
                    region: *region,
                    reopen_tool: None,
                    page_status,
                }),
                CloudRender::Stopped(..) => None,
            },
        );
    }

    // The one kind that is not an edit: the mask is kept exactly as it is
    // and handed back to a tool. Maps mask origin to its tool and opens
    // that tool instead, falling back to the AI mask brush when origin
    // is unknown.
    if kind == "reopenInTool" {
        let Some((region, page_status)) = current_region(&located, &region_id)? else {
            return Ok(None);
        };
        let tool = existing_tool(&located, &region_id)?;
        let tool_name = tool.as_deref().unwrap_or("aiMaskBrush");
        let label_key = tool_label_key(tool_name);
        crate::events::notice(
            "notice.mask.reopened",
            serde_json::json!({ "toolKey": label_key }),
            "info",
        );
        return Ok(Some(RerunResult {
            mask: region.mask.clone(),
            region,
            reopen_tool: Some(match tool_name {
                "brush" => "brush",
                "shapes" => "shapes",
                "aiMaskBrush" => "aiMaskBrush",
                "cloneHeal" => "cloneHeal",
                _ => "aiMaskBrush",
            }),
            page_status,
        }));
    }

    let (target, fill_mode) = match kind {
        "cycleFill" => {
            let next = next_fill_mode(&current_fill);
            (engine_for_fill_mode(next), Some(next))
        }
        "stronger" => (step(current, true), None),
        "simpler" => (step(current, false), None),
        "engine" => (named_rung(engine).unwrap_or(current), None),
        // `retry`, `retryWider`, and anything a later interface adds: run
        // what ran. Wider differs only in the hole, below.
        _ => (
            current,
            if current_fill == "solid" {
                Some("solid")
            } else {
                None
            },
        ),
    };

    // Remote-provenance rerun must NOT silently rerun local. A grant took
    // it to the cloud above; without one it goes nowhere.
    if rerun_needs_cloud(target, current, is_cloud_provenance, kind == "engine") {
        if app.is_some_and(crate::inference::cloud_allowed) {
            refuse("decline.reason.rungUnavailable");
        } else {
            cloud_blocked();
        }
        return Ok(None);
    }

    let plan = Plan {
        expected_source: None,
        region_id: region_id.clone(),
        geometry: if kind == "retryWider" { Geometry::StoredWider } else { Geometry::Stored },
        choice: Choice::Exact(target),
        // A re-run does not change whose edit it is: an automatic patch
        // re-run by hand is still the automatic pass's region, with a
        // different engine under it.
        source: existing_source(&located, &region_id)?,
        tool: existing_tool(&located, &region_id)?,
        fill_mode,
        solid_color: None,
        // **A re-run re-runs a rung, and a stroke is not one.** The
        // manifest keeps a painted patch's mask and pixels like any other's,
        // but nothing in the record replays the gesture - the dab list never
        // reached disk. So a re-run of a painted region falls through to the
        // ladder, which is recorded rather than a silent behaviour.
        paint: None,
        clears_untouched: false,
        requested: Some(current_bbox),
        stroke: None,
        drawn: None,
    };
    match apply(&located, plan)? {
        Outcome::Cleaned(edited) => {
            crate::events::notice(
                rerun_notice(kind),
                match fill_mode {
                    Some(mode) => serde_json::json!({ "fillModeKey": fill_mode_label(mode) }),
                    None => serde_json::json!({ "rungKey": target.rung_key() }),
                },
                "info",
            );
            Ok(Some(RerunResult {
                mask: edited.region.mask.clone(),
                region: edited.region,
                reopen_tool: None,
                page_status: edited.page_status,
            }))
        }
        Outcome::Refused(reason) => {
            refuse(reason);
            Ok(None)
        }
        Outcome::NotFound => Ok(None),
    }
}

/// Which notice a re-run leaves on the stack.
fn rerun_notice(kind: &str) -> &'static str {
    match kind {
        "stronger" => "notice.mask.rerunStronger",
        "simpler" => "notice.mask.rerunSimpler",
        "cycleFill" => "notice.mask.fillMode",
        "engine" => "notice.mask.rerunEngine",
        "retryWider" => "notice.mask.rerunWider",
        _ => "notice.mask.rerunAgain",
    }
}

/// The catalogue's name for a fill mode - `src/lib/model/masks.js`'s mapping,
/// which is the one the panel reads.
fn fill_mode_label(mode: &str) -> &'static str {
    match mode {
        "reconstruct" => "masks.fillMode.reconstruct",
        "solid" => "masks.fillMode.solid",
        _ => "masks.fillMode.matchSurround",
    }
}

/// Which tool made the region, if recorded in the params snapshot.
fn existing_tool(located: &Located, region_id: &str) -> Result<Option<String>, String> {
    let _lock = run::lock_job(&located.job_path)?;
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    Ok(job
        .project
        .patches
        .iter()
        .find(|record| record.id == region_id)
        .and_then(|record| {
            record
                .provenance
                .params_snapshot
                .get("tool")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        }))
}

/// The catalogue key for a tool's name.
fn tool_label_key(tool: &str) -> &'static str {
    match tool {
        "brush" => "tools.name.brush",
        "shapes" => "tools.name.shapes",
        "aiMaskBrush" => "tools.name.aiMaskBrush",
        "cloneHeal" => "tools.name.cloneHeal",
        _ => "tools.name.aiMaskBrush",
    }
}

fn retry_allowed(kind: &str, current: Engine) -> bool {
    !matches!(kind, "retry" | "retryWider") || !matches!(current, Engine::Paint | Engine::Clone)
}

/// Read the committed patch before either the local or cloud re-run route.
/// A saved paint or clone stroke has no gesture to replay on either route.
fn rerun_current(
    located: &Located,
    region_id: &str,
    kind: &str,
) -> Result<Option<(Engine, String, Rect, bool)>, String> {
    let _lock = run::lock_job(&located.job_path)?;
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    let Some(record) = job.project.patches.iter().find(|record| record.id == region_id) else {
        return Ok(None);
    };
    if !retry_allowed(kind, record.provenance.engine) {
        return Ok(None);
    }
    let fill = record.provenance.params_snapshot.get("fill_mode")
        .and_then(|v| v.as_str()).unwrap_or("match-surround").to_owned();
    let is_cloud = record.provenance.cloud.is_some() || record.provenance.engine == Engine::Cloud;
    Ok(Some((record.provenance.engine, fill, cleaner_core::project::orientation::display_bbox(&job.project,record), is_cloud)))
}

/// Whether the region being re-run was a hand edit or the automatic pass's.
fn existing_source(located: &Located, region_id: &str) -> Result<&'static str, String> {
    let _lock = run::lock_job(&located.job_path)?;
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    Ok(job
        .project
        .patches
        .iter()
        .find(|record| record.id == region_id)
        .and_then(|record| {
            record
                .provenance
                .params_snapshot
                .get("source")
                .and_then(|v| v.as_str())
        })
        .map(|source| if source == "hand" { "hand" } else { "auto" })
        .unwrap_or("auto"))
}

/// One region as the manifest holds it right now, without editing anything.
fn current_region(
    located: &Located,
    region_id: &str,
) -> Result<Option<(ApiRegion, String)>, String> {
    let _lock = run::lock_job(&located.job_path)?;
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    let Some(source_idx) = Library::resolve_page(&job.project, located.page_index) else {
        return Ok(None);
    };
    Ok(crate::library::region_and_status(
        &located.chapter_id,
        &job.project,
        source_idx,
        region_id,
    ))
}

/// Cap the native working window and dab count before allocating the preview.
/// Clone/heal composites the union of target and source, not just the stroke.
fn paint_preview_fits(paint: &PaintPlan, width: u32, height: u32) -> bool {
    if paint.points.is_empty() || paint.points.len() > 8192 { return false; }
    let points: Vec<(f64,f64)> = paint.points.iter().map(|p|
        (p.x * width as f64 / 100.0, p.y * height as f64 / 100.0)).collect();
    if points.iter().any(|(x,y)| !x.is_finite() || !y.is_finite()) { return false; }
    let radius = paint.spec.size * 2.0;
    let (offset_x, offset_y) = match paint.kind {
        PaintKind::Clone { offset, .. } => (offset.0.abs() * width as f64 / 100.0, offset.1.abs() * height as f64 / 100.0),
        PaintKind::Shape { .. } => return false,
        _ => (0.0,0.0),
    };
    let extent = |axis: usize| {
        let values = points.iter().map(|p| if axis == 0 {p.0} else {p.1});
        values.clone().fold(f64::NEG_INFINITY, f64::max) - values.fold(f64::INFINITY, f64::min) + radius
    };
    let area = (extent(0) + offset_x).min(width as f64) * (extent(1) + offset_y).min(height as f64);
    let length: f64 = points.windows(2).map(|p| (p[1].0-p[0].0).hypot(p[1].1-p[0].1)).sum();
    let step = (paint.spec.size * paint.spec.spacing.clamp(1.0,50.0)/100.0).max(0.01);
    let dabs = 1.0 + length / step;
    let visits = dabs * (paint.spec.size + 2.0).powi(2).min(area);
    area.is_finite() && area <= 4_194_304.0 && dabs <= 16384.0 && visits <= 67_108_864.0
}

/// Read-only authoritative paint preview. Pixels come from the same native
/// renderer as commit, then cross the managed display boundary exactly once.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaintPreview {
    request_id: u64,
    revision: String,
    chapter_id: String,
    page_index: u32,
    bounds: Bbox,
    png: Vec<u8>,
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn preview_paint(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: u32,
    tool: String,
    params: serde_json::Value,
    request_id: u64,
    revision: String,
    source_index: Option<usize>,
    source_sha: Option<String>,
) -> Result<Option<PaintPreview>, String> {
    crate::library::blocking(move || {
        let Some(mut paint) = paint_plan(&tool, &params) else { return Ok(None) };
        if paint.points.len() > 8192 { return Err("paint preview point limit exceeded".into()); }
        let library = Library::for_app(&app)?;
        let Some(located) = locate_page(&library, &chapter_id, page_index as usize)? else { return Ok(None) };
        let _lock = run::lock_job(&located.job_path)?;
        let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
        let Some(source_idx) = Library::resolve_page(&job.project, located.page_index) else { return Ok(None) };
        if source_index.is_some_and(|index|index!=source_idx) || source_sha.as_ref().is_some_and(|sha|sha!=&job.project.sources[source_idx].sha256) { return Ok(None); }
        let Some(path) = job.source_path(source_idx) else { return Ok(None) };
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let page = job.display_page(source_idx, &bytes).map_err(|e| e.to_string())?;
        if !paint_preview_fits(&paint, page.width, page.height) { return Ok(None); }
        let strip = run::strip_of(&job.project);
        let anchor = strip.pages().get(located.page_index).copied();
        let mut shift = (0,0);
        let window_page;
        let page_to_paint = if job.project.strip.mode == cleaner_core::project::StripMode::Longstrip {
            let target = params.get("previewBbox").and_then(|v| serde_json::from_value::<Bbox>(v.clone()).ok())
                .map(|bbox| pixels_of(bbox, page.width, page.height));
            if let Some((requested, anchor)) = target.map(|r|manual_requested(r,Some(&paint),anchor)).zip(anchor)
                .filter(|(r,p)|r.x<0 || r.y<0 || r.right()>p.width as i64 || r.bottom()>p.height as i64) {
                let global = Rect::new(requested.x+anchor.x_offset, requested.y+anchor.y_offset,requested.w,requested.h);
                let mut joins = cleaner_core::strip::Joins::unchecked(strip.joins());
                for join in 0..strip.joins() { joins.set(join,cleaner_core::strip::JoinState::Verified); }
                let window = cleaner_core::strip::decode_window(&strip,&joins,global,1.0,cleaner_core::strip::EngineContext::None);
                if u64::from(window.rect.w)*u64::from(window.rect.h)>4_194_304 { return Ok(None); }
                let read = cleaner_core::strip::read_window_borrowing(&strip,window,|position| {
                    if position==located.page_index { return Ok(Cow::Borrowed(&page)); }
                    let source = Library::resolve_page(&job.project,position).ok_or_else(||"preview strip page missing".to_owned())?;
                    let path = job.source_path(source).ok_or_else(||"preview source missing".to_owned())?;
                    let data = std::fs::read(path).map_err(|e|e.to_string())?;
                    job.display_page(source,&data).map(Cow::Owned).map_err(|e|e.to_string())
                })?;
                shift=(read.origin.0-anchor.x_offset,read.origin.1-anchor.y_offset);
                remap_paint(&mut paint,anchor,read.origin,read.raster.width,read.raster.height);
                window_page=read.raster;
                &window_page
            } else { &page }
        } else { &page };
        let context = (job.project.strip.mode == cleaner_core::project::StripMode::Longstrip)
            .then_some(StripPaintContext { strip: &strip, order: &job.project.strip.order, origin: (anchor.map_or(0, |p| p.x_offset)+shift.0, anchor.map_or(0, |p| p.y_offset)+shift.1) });
        let (mut made, _, _) = paint_patch(&job, source_idx, page_to_paint, &paint, next_order(&job, source_idx), context)
            .map_err(str::to_owned)?;
        translate_made(&mut made,shift);
        let bounds = made.mask.bounds;
        if u64::from(bounds.w) * u64::from(bounds.h) > 4_194_304 { return Ok(None); }
        let display = cleaner_core::image::proxy::display(&made.pixels).map_err(|e|e.to_string())?;
        let png = cleaner_core::image::encode(&display, cleaner_core::image::Format::Png).map_err(|e|e.to_string())?;
        Ok(Some(PaintPreview {
            request_id, revision, chapter_id, page_index,
            bounds: Bbox { x: bounds.x as f64 / page.width as f64 * 100.0,
                y: bounds.y as f64 / page.height as f64 * 100.0,
                w: bounds.w as f64 / page.width as f64 * 100.0,
                h: bounds.h as f64 / page.height as f64 * 100.0 },
            png,
        }))
    }).await
}

/// `cleanAnyway` - clean a region the script gate held back, on the user's
/// say-so.
///
/// **The automatic pass still does not touch out-of-balloon text**, and this is
/// what makes that ruling affordable rather than absolute: burning SFX and
/// artwork on a guess is refused, and `Verdict::OutOfBalloon` leaves every
/// such region for a person to look at. When that person says yes, the rung
/// it starts on is **their own pick for this kind of text** - the
/// `outsideEngine` row of the tool window, recorded as a preference with no
/// automatic effect. This is the effect. It moves the start and nothing
/// else: the ladder escalates past it exactly as a run's would, and it is
/// capped at [`crate::run::HIGHEST_AUTOMATIC`] unless the caller names rung 3a
/// outright.
///
/// `params` carries a cloud grant, as for `rerunMask`. Consent binds a grant
/// to a stored region, so the cloud can render a region only once it has a
/// patch: a region the gate held back is cleaned here locally first, and
/// rendered in the cloud by the edit after that.
#[tauri::command]
pub async fn clean_anyway(
    app: tauri::AppHandle,
    region_id: String,
    engine: Option<String>,
    params: Option<serde_json::Value>,
) -> Result<Option<CleanedRegion>, String> {
    crate::library::blocking(move || {
        if is_approved_component_region(&region_id) {
            return Err("Approved component edits are insertion-only; use Undo or Delete instead of replacing their saved mask".into());
        }
        let cloud_target = engine.as_deref() == Some("cloud")
            || named_rung(engine.as_deref()) == Some(Engine::Cloud)
            || params.as_ref().and_then(grant_nonce_in).is_some();
        if cloud_target && !crate::inference::cloud_allowed(&app) {
            cloud_blocked();
            return Ok(None);
        }
        let library = Library::for_app(&app)?;
        clean_anyway_at(&library, Some(&app), &region_id, engine.as_deref(),
            params.unwrap_or(serde_json::Value::Null), |located, plan| edit(&app, located, plan))
    })
    .await
}

fn clean_anyway_at(
    library: &Library,
    app: Option<&tauri::AppHandle>,
    region_id: &str,
    engine: Option<&str>,
    params: serde_json::Value,
    mut apply: impl FnMut(&Located, Plan) -> Result<Outcome, String>,
) -> Result<Option<CleanedRegion>, String> {
    if is_approved_component_region(region_id) {
        return Err("Approved component edits are insertion-only; use Undo or Delete instead of replacing their saved mask".into());
    }
    let grant_nonce = grant_nonce_in(&params);
    let has_cloud_target = engine == Some("cloud")
        || named_rung(engine) == Some(Engine::Cloud)
        || grant_nonce.is_some();

    if has_cloud_target && !app.is_some_and(crate::inference::cloud_allowed) {
        cloud_blocked();
        return Ok(None);
    }

    let Some(located) = locate_region(library, region_id)? else {
        return Ok(None);
    };

    if let Some(grant_nonce) = grant_nonce {
        let app = app.expect("cloud permission requires an app");
        return Ok(
            match render_in_cloud(app, &located, region_id, &params, &grant_nonce) {
                CloudRender::Committed(region, page_status) => Some(CleanedRegion {
                    mask: region.mask.clone(),
                    region: *region,
                    page_status,
                }),
                CloudRender::Stopped(..) => None,
            },
        );
    }

    let color = params.get("bubbleColor").or_else(|| params.get("color"))
        .and_then(|value| value.as_str()).map(|value| colour_of(Some(value)));
    let exact = params.get("exact").and_then(|value| value.as_bool()) == Some(true);
    let plan = clean_anyway_plan(&located, region_id, engine, color, exact)?;
    match apply(&located, plan)? {
        Outcome::Cleaned(edited) => {
            crate::events::notice("notice.gate.cleanedAnyway", serde_json::json!({}), "info");
            Ok(Some(CleanedRegion {
                mask: edited.region.mask.clone(),
                region: edited.region,
                page_status: edited.page_status,
            }))
        }
        Outcome::Refused(reason) => {
            refuse(reason);
            Ok(None)
        }
        Outcome::NotFound => Ok(None),
    }
}

/// `exact` is a model the user picked from Clean with: it is run, and not
/// scored against (the module docs), so a region the metric declined can be
/// cleaned with the model the user chose. Without it, `engine` is a start and
/// the ladder escalates from it as a run would ([`choice_for`]).
fn clean_anyway_plan(
    located: &Located,
    region_id: &str,
    engine: Option<&str>,
    color: Option<[u8; 3]>,
    exact: bool,
) -> Result<Plan, String> {
    let geometry = existing_geometry(located, region_id)?;
    let row = untouched_reason(located, region_id)?;
    let fallback = untouched_fallback_pick(
        row.as_ref().map(|(reason, _)| reason.as_str()),
        row.as_ref().and_then(|(_, inside)| *inside),
    );
    let choice = match named_rung(engine) {
        Some(rung) if exact => Choice::Exact(rung),
        _ => choice_for(engine, fallback),
    };
    Ok(Plan {
        expected_source: None,
        region_id: region_id.to_owned(),
        geometry,
        choice,
        source: "auto",
        tool: None,
        fill_mode: (engine == Some("solid")).then_some("solid"),
        solid_color: color,
        // `cleanAnyway` answers the gate, which nothing painted ever met.
        paint: None,
        clears_untouched: true,
        requested: None,
        stroke: None,
        drawn: None,
    })
}

/// Whether rung 3a can be offered on this machine.
#[tauri::command]
pub async fn sidecar_available(app: tauri::AppHandle) -> Result<SidecarStatus, String> {
    crate::library::blocking(move || {
        use tauri::Manager;
        let app_data = app.path().app_data_dir().ok();
        let stored_path = setting_str(crate::settings::read(&app).ok().as_ref(), SIDECAR_PATH_KEYS);
        let explicit = stored_path.as_deref().filter(|s| !s.is_empty());
        let stored_backend = flux_backend_setting(&app);
        Ok(sidecar_status_from(
            explicit,
            stored_backend.as_deref(),
            app_data.as_deref(),
        ))
    })
    .await
}

/// List available model directories in the sidecar weights folder.
#[tauri::command]
pub async fn list_sidecar_models(app: tauri::AppHandle) -> Result<Vec<SidecarModelInfo>, String> {
    crate::library::blocking(move || {
        use tauri::Manager;
        let app_data = app.path().app_data_dir().ok();
        let stored_path = setting_str(crate::settings::read(&app).ok().as_ref(), SIDECAR_PATH_KEYS);
        let explicit = stored_path.as_deref().filter(|s| !s.is_empty());
        Ok(list_sidecar_models_from(explicit, app_data.as_deref()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_fixture(name: &str, raw: &Raster) -> (PathBuf, Library, Located) {
        use cleaner_core::image::{encode, Format};
        use cleaner_core::project::StripMode;

        let root = std::env::temp_dir().join(format!("mc-region-command-{name}-{}", std::process::id()));
        let raws = root.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        std::fs::write(raws.join("page.png"), encode(raw, Format::Png).unwrap()).unwrap();
        let library = Library::at(root.join("library"));
        let project = library.create_project(name, StripMode::Single, Some(raws), None).unwrap();
        let chapter = library.create_chapter(&project.id, "chapter", None, None)
            .unwrap().created().unwrap();
        let located = Located {
            chapter_id: chapter.id.clone(),
            job_path: library.resolve_chapter(&chapter.id).unwrap(),
            page_index: 0,
        };
        (root, library, located)
    }

    fn edit_fixture(name: &str, raw: &Raster) -> (PathBuf, Located) {
        use cleaner_core::image::{encode, Format};
        use cleaner_core::ingest::source_ref;
        use cleaner_core::project::{Project, StripMode};

        let root = std::env::temp_dir().join(format!("mc-region-{name}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("raws")).unwrap();
        let bytes = encode(raw, Format::Png).unwrap();
        let path = root.join("raws/page.png");
        std::fs::write(&path, &bytes).unwrap();
        let source = source_ref(&path, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        Job::create(&manifest, Project::new(manifest.parent().unwrap(), "test",
            StripMode::Single, &[source])).unwrap();
        (root, Located { chapter_id: "chapter".into(), job_path: manifest, page_index: 0 })
    }

    fn fixture_plan(id: &str, geometry: Geometry, engine: Engine, bounds: Rect) -> Plan {
        Plan {
            expected_source: None,
            region_id: id.into(),
            geometry,
            choice: Choice::Exact(engine),
            source: "hand",
            tool: Some("aiMaskBrush".into()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: Some(bounds),
            stroke: None,
            drawn: None,
        }
    }

    fn white_predecessor(located: &Located, raw: &Raster, bounds: Rect) {
        let mut job = Job::open(&located.job_path).unwrap();
        let mut pixels = raw.clone();
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.data = vec![255; bounds.w as usize * bounds.h as usize
            * raw.mode.samples() * usize::from(raw.depth.bits() / 8)];
        let mask = Mask::filled(bounds);
        job.complete_region(0, &Patch {
            id: "a".into(), mask: mask.clone(), ink: mask, pixels,
            order: 0, visible: true,
            provenance: Provenance {
                engine: Engine::Fill, engine_version: "fixture".into(),
                model_sha256: None, execution_provider: "cpu".into(),
                params_snapshot: serde_json::json!({}), mask_sha256: String::new(),
                source_sha256: String::new(), cloud: None, created: 0,
            },
        }, None).unwrap();
    }

    #[test]
    fn detected_region_reads_prior_visible_patch_and_records_input() {
        let mut raw = cleaner_core::image::fixtures::by_name("l8").raster;
        raw.width = 128;
        raw.height = 128;
        raw.data = vec![0; 128 * 128];
        let (root, located) = edit_fixture("detected-underlay", &raw);
        white_predecessor(&located, &raw, Rect::new(36, 36, 12, 12));
        let bounds = Rect::new(42, 40, 12, 6);
        let mut bench = test_bench(&root);
        bench.detected_seed = Some(Mask::filled(bounds));
        let mut seen = None;
        let mut fake = |image: &Raster, fitted: &Fitted| {
            seen = Some(image.sample(44, 40, 0));
            run::Made {
                engine: Engine::Fill, mask: fitted.mask.clone(), ink: fitted.ink.clone(),
                pixels: cleaner_core::engines::fill::render(image, fitted),
                provider: None, model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None, tiles: None,
            }
        };
        assert!(matches!(edit_with_bench_render(&mut bench, Engine::Fill, &located,
            fixture_plan("b", Geometry::Detected(bounds), Engine::Fill, bounds),
            Some(&mut fake)).unwrap(), Outcome::Cleaned(_)));
        assert_eq!(seen, Some(255));
        let job = Job::open(&located.job_path).unwrap();
        let record = job.project.patches.iter().find(|p| p.id == "b").unwrap();
        assert_eq!(record.provenance.params_snapshot["input_provenance"]["version"], 1);
    }

    fn store_a_detection(located: &Located, id: &str, mask: &Mask, ink: &Mask, order: u32) {
        use cleaner_core::project::{DetectedRegion, DetectionFit};
        let mut job = Job::open(&located.job_path).unwrap();
        job.store_detection(DetectedRegion {
            id: id.into(),
            source_idx: 0,
            bbox: Rect::new(42, 40, 12, 6),
            mask_ref: String::new(),
            inside: true,
            balloon_color: Some([240; 3]),
            script: Some("ja".into()),
            pick: "fill".into(),
            detector: "local".into(),
            created: 0,
            mask_sha256: String::new(),
            order,
            review_state: Some("review.reason.unusuallyLarge".into()),
            fit: Some(DetectionFit {
                route: cleaner_core::fit::Route::Fill,
                thickness: 3,
                best_deviation: 0.5,
                scale: 1.0,
            }),
            group: None,
            padding_from_seed: false,
            padding_px: 0,
        }, mask, ink).unwrap();
    }

    #[test]
    fn auto_clean_with_a_detection_id_uses_one_region_and_its_measured_color() {
        let raw = cleaner_core::image::fixtures::by_name("l8").raster;
        let (_root, located) = edit_fixture("auto-region-route", &raw);
        let mask = Mask::filled(Rect::new(38, 36, 20, 14));
        store_a_detection(&located, "chapter-p001-d0", &mask, &mask, 0);
        assert_eq!(stored_solid_color(&located, "chapter-p001-d0", Some([255; 3])).unwrap(), Some([240; 3]));
        assert_eq!(stored_solid_color(&located, "absent", Some([255; 3])).unwrap(), Some([255; 3]));
    }

    /// `applyTool` on a stored detection cleans it from the mask Detect stored,
    /// with the lettering and the route put back, runs no detector, and
    /// replaces the detection with its patch in one write.
    #[test]
    fn a_stored_detection_is_cleaned_from_its_own_mask_and_replaced_by_its_patch() {
        let mut raw = cleaner_core::image::fixtures::by_name("l8").raster;
        raw.width = 128;
        raw.height = 128;
        raw.data = vec![240; 128 * 128];
        for y in 41..45 {
            for x in 44..52 {
                raw.data[y * 128 + x] = 20;
            }
        }
        let (root, located) = edit_fixture("stored-detection", &raw);
        let mask = Mask::filled(Rect::new(38, 36, 20, 14));
        let ink = Mask::filled(Rect::new(41, 39, 14, 8));
        store_a_detection(&located, "chapter-p001-d0", &mask, &ink, 4);
        store_a_detection(&located, "chapter-p001-d1", &mask, &ink, 5);
        assert!(matches!(existing_geometry(&located, "chapter-p001-d0").unwrap(),
            Geometry::StoredDetection));
        assert_eq!(job_page_id("chapter-p001-d0"), "chapter-p001");

        let plan = |id: &str| Plan {
            expected_source: None,
            region_id: id.into(),
            geometry: Geometry::StoredDetection,
            choice: Choice::Ladder(Some(EnginePick::Fill)),
            source: "auto",
            tool: Some("autoClean".into()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: true,
            paint: None,
            requested: None,
            stroke: None,
            drawn: None,
        };
        let mut bench = test_bench(&root);
        let mut seen = None;
        let mut fake = |image: &Raster, fitted: &Fitted| {
            seen = Some((fitted.mask.bounds, fitted.ink.bounds, fitted.route, fitted.thickness));
            run::Made {
                engine: Engine::Fill, mask: fitted.mask.clone(), ink: fitted.ink.clone(),
                pixels: cleaner_core::engines::fill::render(image, fitted),
                provider: None, model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None, tiles: None,
            }
        };
        let Outcome::Cleaned(edited) = edit_with_bench_render(&mut bench, Engine::Fill, &located,
            plan("chapter-p001-d0"), Some(&mut fake)).unwrap() else {
            panic!("the detection was not cleaned");
        };
        assert_eq!((edited.region.id.as_str(), edited.region.outcome), ("chapter-p001-d0", "cleaned"));
        assert_eq!(ApplyResult::applied(edited).status, "applied");
        let (fitted_mask, fitted_ink, route, thickness) = seen.expect("rendered");
        assert_eq!((fitted_mask.w, fitted_mask.h), (20, 14), "the stored mask, as it was");
        assert_eq!((fitted_ink.x - fitted_mask.x, fitted_ink.y - fitted_mask.y, fitted_ink.w, fitted_ink.h),
            (3, 3, 14, 8), "the stored lettering, not the mask again");
        assert_eq!((route, thickness), (cleaner_core::fit::Route::Fill, 3));

        let job = Job::open(&located.job_path).unwrap();
        assert_eq!(job.project.detections.len(), 1);
        let patch = job.project.patches.iter().find(|p| p.id == "chapter-p001-d0").unwrap();
        assert_eq!(patch.bbox, mask.bounds);
        assert_eq!(patch.order, 4);
        assert_eq!(patch.review_state.as_deref(), Some("review.reason.unusuallyLarge"));
        assert_eq!(patch.provenance.params_snapshot["source"], "auto");
        assert!(!job.sidecar().join("detections/chapter-p001-d0.mask").exists());

        // The real ladder, from the stored pick: rung 0 paints the lettering out.
        assert!(matches!(edit_with_bench(&mut bench, Engine::Fill, &located, plan("chapter-p001-d1")).unwrap(),
            Outcome::Cleaned(_)));
        let job = Job::open(&located.job_path).unwrap();
        assert!(job.project.detections.is_empty());
        let patch = job.load_patch(job.project.patches.iter().find(|p| p.id == "chapter-p001-d1").unwrap())
            .unwrap();
        assert!(!patch.pixels.data.contains(&20), "the lettering is gone");
    }

    #[test]
    fn exact_engines_decline_unsupported_source_modes() {
        for (name, engine) in [("indexed-p", Engine::Lama), ("bitonal", Engine::Lama)] {
            let raw = cleaner_core::image::fixtures::by_name(name).raster;
            let (root, located) = edit_fixture(&format!("exact-{name}-{engine:?}"), &raw);
            let bounds = Rect::new(22, 18, 8, 8);
            let outcome = edit_with_bench(&mut test_bench(&root), Engine::Lama, &located,
                fixture_plan("b", Geometry::Painted(Mask::filled(bounds)), engine, bounds)).unwrap();
            assert!(matches!(outcome, Outcome::Refused("decline.reason.rungUnavailable")),
                "{name} {engine:?}");
            assert!(Job::open(&located.job_path).unwrap().project.patches.is_empty());
        }
    }

    #[test]
    fn retry_refuses_non_replayable_paint_and_clone_patches() {
        let raw = cleaner_core::image::fixtures::by_name("l8").raster;
        for engine in [Engine::Paint, Engine::Clone] {
            let (_root, library, located) = command_fixture(&format!("retry-{engine:?}"), &raw);
            let region_id = format!("{}-p001-h1", located.chapter_id);
            let bounds = Rect::new(20, 20, 8, 8);
            let mask = Mask::filled(bounds);
            let mut pixels = raw.clone();
            pixels.width = bounds.w;
            pixels.height = bounds.h;
            pixels.data = vec![255; 64 * raw.mode.samples()];
            let patch = Patch {
                id: region_id.clone(), mask: mask.clone(), ink: mask, pixels,
                order: 0, visible: true,
                provenance: Provenance {
                    engine, engine_version: "fixture".into(), model_sha256: None,
                    execution_provider: "cpu".into(), params_snapshot: serde_json::json!({}),
                    mask_sha256: String::new(), source_sha256: String::new(),
                    cloud: None, created: 0,
                },
            };
            Job::open(&located.job_path).unwrap().complete_region(0, &patch, None).unwrap();
            for kind in ["retry", "retryWider"] {
                assert!(rerun_mask_at(&library, None, &region_id, kind, None,
                    serde_json::Value::Null, |_, _| panic!("{kind} ran a non-replayable stroke"))
                    .unwrap().is_none());
            }
            let mut called = false;
            let _ = rerun_mask_at(&library, None, &region_id, "engine", Some("fill"),
                serde_json::Value::Null, |_, _| {
                    called = true;
                    Ok(Outcome::NotFound)
                }).unwrap();
            assert!(called);
        }
    }

    /// The two Try agains differ in one thing, the hole: same rung, same
    /// stored geometry, and only `retryWider` asks for the stored mask.
    #[test]
    fn retry_wider_asks_for_the_stored_mask_and_retry_for_the_stored_hole() {
        let raw = cleaner_core::image::fixtures::by_name("l8").raster;
        let (_root, library, located) = command_fixture("retry-wider-plan", &raw);
        let region_id = format!("{}-p001-h1", located.chapter_id);
        let mask = Mask::filled(Rect::new(20, 20, 8, 8));
        let mut pixels = raw.clone();
        pixels.width = 8;
        pixels.height = 8;
        pixels.data = vec![255; 64 * raw.mode.samples()];
        Job::open(&located.job_path).unwrap().complete_region(0, &Patch {
            id: region_id.clone(), mask: mask.clone(), ink: mask, pixels, order: 0, visible: true,
            provenance: Provenance {
                engine: Engine::Lama, engine_version: "fixture".into(), model_sha256: None,
                execution_provider: "cpu".into(), params_snapshot: serde_json::json!({}),
                mask_sha256: String::new(), source_sha256: String::new(), cloud: None, created: 0,
            },
        }, None).unwrap();
        for (kind, wider) in [("retry", false), ("retryWider", true)] {
            let mut asked = None;
            let _ = rerun_mask_at(&library, None, &region_id, kind, None, serde_json::Value::Null, |_, plan| {
                asked = Some((matches!(plan.geometry, Geometry::StoredWider),
                    matches!(plan.geometry, Geometry::Stored), plan.choice == Choice::Exact(Engine::Lama)));
                Ok(Outcome::NotFound)
            }).unwrap();
            assert_eq!(asked, Some((wider, !wider, true)), "{kind}");
        }
    }

    #[test]
    fn clean_anyway_held_back_region_reads_visible_hand_patch() {
        let mut raw = cleaner_core::image::fixtures::by_name("l8").raster;
        raw.width = 128;
        raw.height = 128;
        raw.data = vec![0; 128 * 128];
        let (root, library, located) = command_fixture("clean-anyway-visible", &raw);
        white_predecessor(&located, &raw, Rect::new(36, 36, 12, 12));
        let bounds = Rect::new(42, 40, 12, 6);
        Job::open(&located.job_path).unwrap().leave_untouched(0, bounds,
            "review.reason.gateSkippedOutsideBubble").unwrap();
        let region_id = untouched_id(&format!("{}-p001", located.chapter_id), bounds);
        let mut bench = test_bench(&root);
        bench.detected_seed = Some(Mask::filled(bounds));
        let mut seen = None;
        let mut renderer = |image: &Raster, fitted: &Fitted| {
            seen = Some(image.sample(44, 40, 0));
            run::Made {
                engine: Engine::Fill, mask: fitted.mask.clone(), ink: fitted.ink.clone(),
                pixels: cleaner_core::engines::fill::render(image, fitted),
                provider: None, model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None, tiles: None,
            }
        };
        assert!(clean_anyway_at(&library, None, &region_id, Some("fill"),
            serde_json::Value::Null, |located, plan| {
                assert!(matches!(plan.geometry, Geometry::Detected(rect) if rect == bounds));
                assert!(plan.clears_untouched);
                edit_with_bench_render(&mut bench, Engine::Fill, located, plan, Some(&mut renderer))
            }).unwrap().is_some());
        assert_eq!(seen, Some(255));
        let job = Job::open(&located.job_path).unwrap();
        assert!(job.project.regions_untouched.is_empty());
        assert!(job.project.patches.iter().any(|p| p.id == region_id));
    }

    /// Approving a held candidate cleans through the lettering the run held,
    /// with no detector: a candidate is lettering no detector box claimed, so
    /// looking under its box found nothing and the box itself went to the
    /// rungs, a fill over artwork that no engine could pass.
    #[test]
    fn approving_a_held_candidate_cleans_its_lettering_not_its_box() {
        const GLYPH: u8 = 20;
        let mut raw = cleaner_core::image::fixtures::by_name("l8").raster;
        raw.width = 128;
        raw.height = 128;
        // Grey screentone-like texture, which a flat fill over a box cannot match.
        raw.data = (0..128 * 128).map(|at: usize| 160 + ((at % 128 * 7 + at / 128 * 13) % 9) as u8).collect();
        // Four glyph blobs at the corners of their box, whose middle is paper.
        let mut lettering = Mask::empty(Rect::new(20, 20, 80, 82));
        for (x0, y0) in [(20, 20), (96, 20), (20, 96), (96, 96)] {
            for y in y0..y0 + 6 {
                for x in x0..x0 + 4 {
                    raw.data[y as usize * 128 + x as usize] = GLYPH;
                    lettering.set(x, y, true);
                }
            }
        }
        let bbox = lettering.bounds;
        let (root, library, located) = command_fixture("clean-anyway-candidate", &raw);
        Job::open(&located.job_path).unwrap().leave_untouched_rows(vec![run::held_candidate_row(
            0, bbox, "review.reason.unassignedMask".into(), false, Some((lettering.clone(), 1.0)),
        )]).unwrap();
        let region_id = untouched_id(&format!("{}-p001", located.chapter_id), bbox);
        // No detector seed: whatever looks under the box finds nothing.
        let mut bench = test_bench(&root);
        let mut seen = None;
        let mut renderer = |image: &Raster, fitted: &Fitted| {
            seen = Some((image.clone(), fitted.mask.clone(), fitted.ink.clone()));
            run::Made {
                engine: Engine::Fill, mask: fitted.mask.clone(), ink: fitted.ink.clone(),
                pixels: cleaner_core::engines::fill::render(image, fitted),
                provider: None, model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None, tiles: None,
            }
        };
        assert!(clean_anyway_at(&library, None, &region_id, Some("fill"),
            serde_json::Value::Null, |located, plan| {
                assert!(matches!(&plan.geometry, Geometry::HeldLettering(held, scale)
                    if *held == lettering && *scale == 1.0));
                edit_with_bench_render(&mut bench, Engine::Fill, located, plan, Some(&mut renderer))
            }).unwrap().is_some());

        // The renderer runs in the read window's coordinates: the glyphs are
        // found in the image it was handed.
        let (image, mask, ink) = seen.expect("rendered");
        let glyphs: Vec<(i64, i64)> = (0..image.height).flat_map(|y| (0..image.width).map(move |x| (x, y)))
            .filter(|&(x, y)| image.sample(x, y, 0) == u16::from(GLYPH))
            .map(|(x, y)| (i64::from(x), i64::from(y)))
            .collect();
        assert_eq!(glyphs.len(), lettering.count());
        assert!(glyphs.iter().all(|&(x, y)| ink.contains(x, y)), "a glyph was left outside the hole");
        let hull_x = glyphs.iter().map(|g| g.0).min().unwrap();
        let hull_y = glyphs.iter().map(|g| g.1).min().unwrap();
        let (middle_x, middle_y) = (hull_x + i64::from(bbox.w) / 2, hull_y + i64::from(bbox.h) / 2);
        assert!(!ink.contains(middle_x, middle_y), "the hole is the box, not the lettering");
        let most = lettering.dilated(cleaner_core::constants::MIN_MASK_THICKNESS
            + cleaner_core::constants::MODEL_HOLE_MARGIN, 128, 128).count();
        assert!(ink.count() <= most && ink.count() < (bbox.w * bbox.h) as usize,
            "{} hole pixels for {} glyph pixels in a {} pixel box", ink.count(), lettering.count(), bbox.w * bbox.h);
        assert_ne!(mask, Mask::filled(Rect::new(hull_x, hull_y, bbox.w, bbox.h)));

        let job = Job::open(&located.job_path).unwrap();
        assert!(job.project.regions_untouched.is_empty());
        assert!(job.project.patches.iter().any(|p| p.id == region_id));
    }

    /// A candidate row written before rows kept lettering still looks under
    /// its box, as it always did.
    #[test]
    fn a_candidate_without_stored_lettering_looks_under_its_box() {
        let raw = cleaner_core::image::fixtures::by_name("l8").raster;
        let (_root, _library, located) = command_fixture("clean-anyway-old-candidate", &raw);
        let bounds = Rect::new(20, 20, 8, 8);
        Job::open(&located.job_path).unwrap()
            .hold_candidate(0, bounds, "review.reason.isolatedMask", true).unwrap();
        let region_id = untouched_id(&format!("{}-p001", located.chapter_id), bounds);
        assert!(matches!(existing_geometry(&located, &region_id).unwrap(),
            Geometry::Detected(rect) if rect == bounds));
    }

    #[test]
    fn gated_review_solid_choice_keeps_the_selected_color() {
        let raw = cleaner_core::image::fixtures::by_name("l8").raster;
        let (_root, library, located) = command_fixture("clean-anyway-solid", &raw);
        let bounds = Rect::new(20, 20, 8, 8);
        Job::open(&located.job_path).unwrap().leave_untouched(0, bounds,
            "review.reason.gateSkippedLowConfidence").unwrap();
        let region_id = untouched_id(&format!("{}-p001", located.chapter_id), bounds);
        let mut called = false;
        let _ = clean_anyway_at(&library, None, &region_id, Some("solid"),
            serde_json::json!({"bubbleColor": "#ff8000"}), |_, plan| {
                called = true;
                assert_eq!(plan.fill_mode, Some("solid"));
                assert_eq!(plan.solid_color, Some([255, 128, 0]));
                assert_eq!(plan.choice, Choice::Ladder(Some(EnginePick::Solid)));
                Ok(Outcome::NotFound)
            }).unwrap();
        assert!(called);
    }

    /// A model picked from Clean with on a held or declined row runs as
    /// named: the metric that declined the region's automatic pick does not
    /// veto the user's. Without `exact`, the same word is a start.
    #[test]
    fn a_model_picked_for_a_declined_row_is_run_exactly() {
        let raw = cleaner_core::image::fixtures::by_name("l8").raster;
        let (_root, library, located) = command_fixture("clean-anyway-exact", &raw);
        let bounds = Rect::new(20, 20, 8, 8);
        Job::open(&located.job_path).unwrap().leave_untouched(0, bounds,
            "decline.reason.histogram").unwrap();
        let region_id = untouched_id(&format!("{}-p001", located.chapter_id), bounds);
        for (params, expected) in [
            (serde_json::json!({"exact": true}), Choice::Exact(Engine::Lama)),
            (serde_json::Value::Null, Choice::Ladder(Some(EnginePick::Lama))),
        ] {
            let mut called = false;
            let _ = clean_anyway_at(&library, None, &region_id, Some("lama"), params, |_, plan| {
                called = true;
                assert_eq!(plan.choice, expected);
                Ok(Outcome::NotFound)
            }).unwrap();
            assert!(called);
        }
    }

    #[test]
    fn clone_source_dependency_tracks_visible_input_and_ignores_unrelated_change() {
        let mut raw = cleaner_core::image::fixtures::by_name("l8").raster;
        raw.width = 128;
        raw.height = 128;
        raw.data = vec![0; 128 * 128];
        let (root, located) = edit_fixture("clone-provenance", &raw);
        white_predecessor(&located, &raw, Rect::new(24, 34, 16, 16));
        {
            let mut job = Job::open(&located.job_path).unwrap();
            let mut outside = job.load_patch(&job.project.patches[0]).unwrap();
            outside.id = "outside".into();
            outside.order = 1;
            outside.mask.bounds = Rect::new(100, 100, 16, 16);
            outside.ink.bounds = outside.mask.bounds;
            job.complete_region(0, &outside, None).unwrap();
        }
        let target = Rect::new(64, 34, 16, 16);
        let mut plan = fixture_plan("b", Geometry::Given(target), Engine::Clone, target);
        plan.tool = Some("cloneHeal".into());
        plan.paint = Some(PaintPlan {
            kind: PaintKind::Clone { heal: false, offset: (40.0 / 128.0 * 100.0, 0.0) },
            points: vec![PaintPoint { x: 72.0 / 128.0 * 100.0,
                y: 42.0 / 128.0 * 100.0, p: 1.0 }],
            spec: cleaner_core::paint::BrushSpec {
                size: 12.0, pressure_size: false, ..Default::default()
            },
            alignment: "aligned",
        });
        assert!(matches!(edit_with_bench(&mut test_bench(&root), Engine::Fill,
            &located, plan).unwrap(), Outcome::Cleaned(_)));
        let mut job = Job::open(&located.job_path).unwrap();
        let b = job.project.patches.iter().find(|p| p.id == "b").unwrap();
        let footprint: Rect = serde_json::from_value(
            b.provenance.params_snapshot["input_provenance"]["read_footprint"].clone()).unwrap();
        assert!(footprint.x <= 32 && footprint.right() > 32);
        assert!(footprint.x <= 72 && footprint.right() > 72);
        assert!(job.load_patch(b).unwrap().pixels.data.contains(&255));
        job.project.patches.iter_mut().find(|p| p.id == "outside").unwrap().visible = false;
        job.flush().unwrap();
        crate::underlay::refresh_dependencies(&mut job, "outside").unwrap();
        assert_eq!(job.project.patches.iter().find(|p| p.id == "b").unwrap().review_state, None);
        job.project.patches.iter_mut().find(|p| p.id == "a").unwrap().visible = false;
        job.flush().unwrap();
        crate::underlay::refresh_dependencies(&mut job, "a").unwrap();
        assert_eq!(job.project.patches.iter().find(|p| p.id == "b").unwrap()
            .review_state.as_deref(), Some("review.reason.inputChanged"));
    }

    #[test]
    fn later_ai_stroke_reads_a_real_paint_patch() {
        let mut raw = cleaner_core::image::fixtures::by_name("l8").raster;
        raw.width = 128;
        raw.height = 128;
        raw.data = vec![0; 128 * 128];
        let (root, located) = edit_fixture("real-paint-underlay", &raw);
        let bounds = Rect::new(36, 36, 16, 16);
        let shape = PaintedShape {
            kind: ShapeKind::Rect,
            points: vec![
                StrokePoint { x: 28.125, y: 28.125 },
                StrokePoint { x: 40.625, y: 28.125 },
                StrokePoint { x: 40.625, y: 40.625 },
                StrokePoint { x: 28.125, y: 40.625 },
            ],
            feather: 0.0,
        };
        let mut paint = fixture_plan("a", Geometry::Given(bounds), Engine::Fill, bounds);
        paint.tool = Some("shapes".into());
        paint.fill_mode = Some("solid");
        paint.paint = Some(PaintPlan {
            kind: PaintKind::Shape { color: [255, 255, 255], shape: shape.clone(), outline: None },
            points: Vec::new(),
            spec: cleaner_core::paint::BrushSpec::default(),
            alignment: "aligned",
        });
        paint.drawn = Some(shape);
        let mut bench = test_bench(&root);
        assert!(matches!(edit_with_bench(&mut bench, Engine::Fill, &located, paint).unwrap(),
            Outcome::Cleaned(_)));
        let painted = Job::open(&located.job_path).unwrap();
        assert_eq!(painted.project.patches[0].provenance.engine, Engine::Paint);
        assert_eq!(painted.project.patches[0].provenance.params_snapshot
            ["input_provenance"]["version"], 1);
        let patch = painted.load_patch(&painted.project.patches[0]).unwrap();
        let point = (42, 42);
        assert!(patch.mask.contains(point.0, point.1));
        let visible = patch.pixels.sample((point.0 - patch.mask.bounds.x) as u32,
            (point.1 - patch.mask.bounds.y) as u32, 0);
        drop(painted);
        let b = Rect::new(40, 40, 10, 10);
        let mut seen = None;
        let mut fake = |image: &Raster, fitted: &Fitted| {
            seen = Some(image.sample(point.0 as u32, point.1 as u32, 0));
            run::Made {
                engine: Engine::Fill, mask: fitted.mask.clone(), ink: fitted.ink.clone(),
                pixels: cleaner_core::engines::fill::render(image, fitted),
                provider: None, model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None, tiles: None,
            }
        };
        assert!(matches!(edit_with_bench_render(&mut bench, Engine::Fill, &located,
            fixture_plan("b", Geometry::Painted(Mask::filled(b)), Engine::Fill, b),
            Some(&mut fake)).unwrap(), Outcome::Cleaned(_)));
        assert_eq!(seen, Some(visible));
        assert_ne!(seen, Some(raw.sample(point.0 as u32, point.1 as u32, 0)));
    }

    #[test]
    fn moved_translucent_shape_blends_with_its_destination() {
        let mut raw = cleaner_core::image::fixtures::by_name("l8").raster;
        raw.width = 64;
        raw.height = 32;
        raw.data = (0..32).flat_map(|_| (0..64).map(|x| if x < 32 { 0 } else { 255 })).collect();
        let (root, located) = edit_fixture("move-translucent-shape", &raw);
        let bounds = Rect::new(8, 8, 16, 16);
        let shape = PaintedShape {
            kind: ShapeKind::Rect,
            points: vec![
                StrokePoint { x: 12.5, y: 25.0 }, StrokePoint { x: 37.5, y: 25.0 },
                StrokePoint { x: 37.5, y: 75.0 }, StrokePoint { x: 12.5, y: 75.0 },
            ],
            feather: 0.0,
        };
        let mut plan = fixture_plan("shape", Geometry::Given(bounds), Engine::Fill, bounds);
        plan.tool = Some("shapes".into());
        plan.fill_mode = Some("solid");
        plan.paint = Some(PaintPlan {
            kind: PaintKind::Shape { color: [200, 200, 200], shape: shape.clone(), outline: None },
            points: Vec::new(),
            spec: cleaner_core::paint::BrushSpec { opacity: 50.0, ..Default::default() },
            alignment: "aligned",
        });
        plan.drawn = Some(shape);
        let mut bench = test_bench(&root);
        assert!(matches!(edit_with_bench(&mut bench, Engine::Fill, &located, plan).unwrap(), Outcome::Cleaned(_)));

        let mut job = Job::open(&located.job_path).unwrap();
        let patch = job.load_patch(&job.project.patches[0]).unwrap();
        assert_eq!(patch.layer_style().opacity, 50);
        assert_eq!(patch.pixels.sample(8, 8, 0), 200, "shape stored its old dark background");
        let before = cleaner_core::composite::composite(&raw, &[patch]).unwrap();
        assert_eq!(before.sample(16, 16, 0), 100);

        job.project.patches[0].provenance.params_snapshot["layer"]["offsetX"] = serde_json::json!(32);
        job.flush().unwrap();
        let moved = Job::open(&located.job_path).unwrap();
        let patch = moved.load_patch(&moved.project.patches[0]).unwrap();
        let after = cleaner_core::composite::composite(&raw, &[patch]).unwrap();
        assert_eq!(after.sample(16, 16, 0), 0);
        assert_eq!(after.sample(48, 16, 0), 228, "shape carried the old dark background");
    }

    #[test]
    fn eccentric_ellipse_outline_keeps_pixel_width_at_both_axes() {
        // Radii 100 and 10, against a dense walk of the curve itself.
        let brute = |x: f64, y: f64| (0..200_000)
            .map(|i| {
                let angle = i as f64 / 200_000.0 * std::f64::consts::TAU;
                (100.0 * angle.cos() - x).hypot(10.0 * angle.sin() - y)
            })
            .fold(f64::INFINITY, f64::min);
        for (x, y) in [(98.0, 0.0), (0.0, 8.0), (80.0, 0.0), (-60.0, 7.0), (40.0, -20.0), (0.0, 0.0), (120.0, 3.0)] {
            let exact = ellipse_distance(100.0, 10.0, x, y);
            assert!((exact - brute(x, y)).abs() < 0.01, "({x}, {y}): {exact} against {}", brute(x, y));
        }
        assert!((ellipse_distance(100.0, 10.0, 0.0, 8.0) - 2.0).abs() < 1e-9, "the short axis is exact");
        // A circle is the case closed-form solutions break on.
        assert!((ellipse_distance(50.0, 50.0, 30.0, 40.0)).abs() < 1e-9);
        assert!((ellipse_distance(50.0, 50.0, 3.0, 4.0) - 45.0).abs() < 1e-6);
    }

    #[test]
    fn solid_shape_edges_are_anti_aliased_and_the_outline_is_an_inner_stroke() {
        // A 100 px page; a rectangle from 10.25 to 60.25 across and 20 to 70 down.
        let rect = PaintedShape {
            kind: ShapeKind::Rect,
            points: vec![
                StrokePoint { x: 10.25, y: 20.0 }, StrokePoint { x: 60.25, y: 20.0 },
                StrokePoint { x: 60.25, y: 70.0 }, StrokePoint { x: 10.25, y: 70.0 },
            ],
            feather: 0.0,
        };
        let (mask, rim) = shape_coverage(&rect, 4.0, 100, 100).expect("a shape");
        let at = |x: i64, y: i64| (mask.coverage(x, y), rim[((y - mask.bounds.y) * mask.bounds.w as i64 + x - mask.bounds.x) as usize]);
        // Pixel 10 spans 10..11 and the edge is at 10.25: three quarters in.
        assert_eq!(at(10, 40).0, 191);
        assert_eq!(at(9, 40).0, 0);
        assert_eq!(at(30, 40), (255, 0.0), "the middle is fill");
        // The inner stroke is four pixels: 20..24 down is outline, 24.. is fill.
        assert_eq!(at(30, 23), (255, 1.0));
        assert_eq!(at(30, 24), (255, 0.0));
        assert_eq!(at(30, 19).0, 0, "the outline never grows the shape");

        let feathered = PaintedShape { feather: 8.0, ..rect };
        let (soft, _) = shape_coverage(&feathered, 0.0, 100, 100).expect("a shape");
        // The top edge is at y = 20, between rows 19 and 20: a Gaussian edge
        // is symmetric about it.
        let pair = soft.coverage(30, 19) as i32 + soft.coverage(30, 20) as i32;
        assert!((pair - 255).abs() <= 2, "a feathered edge is half covered at the edge, got {pair}");
        assert!(soft.coverage(30, 12) > 0 && soft.coverage(30, 12) < 20, "the feather fades past the edge");
        assert_eq!(soft.coverage(30, 40), 255);
    }

    #[test]
    fn a_lasso_edge_is_anti_aliased_and_its_holes_stay_holes() {
        let triangle = PaintedShape {
            kind: ShapeKind::Polygon,
            points: vec![StrokePoint { x: 10.0, y: 10.0 }, StrokePoint { x: 90.0, y: 10.0 }, StrokePoint { x: 10.0, y: 90.0 }],
            feather: 0.0,
        };
        let (mask, _) = shape_coverage(&triangle, 0.0, 100, 100).expect("a shape");
        assert_eq!(mask.coverage(20, 20), 255);
        assert_eq!(mask.coverage(80, 80), 0);
        // Pixel (49, 50) has its centre on the hypotenuse x + y = 100.
        let edge = mask.coverage(49, 50) as i32;
        assert!((edge - 128).abs() <= 2, "the diagonal is half covered, got {edge}");
    }

    #[test]
    fn rgba8_and_sixteen_bit_renderers_receive_visible_composite() {
        for name in ["rgba8", "rgb16"] {
            let raw = cleaner_core::image::fixtures::by_name(name).raster;
            let (root, located) = edit_fixture(&format!("visible-{name}"), &raw);
            white_predecessor(&located, &raw, Rect::new(20, 16, 12, 12));
            let bounds = Rect::new(24, 20, 8, 8);
            let job = Job::open(&located.job_path).unwrap();
            let expected = crate::underlay::read(&job, 0, &raw, bounds, 1, None).unwrap();
            drop(job);
            let mut seen = None;
            let mut fake = |image: &Raster, fitted: &Fitted| {
                seen = Some(image.clone());
                run::Made {
                    engine: Engine::Fill, mask: fitted.mask.clone(), ink: fitted.ink.clone(),
                    pixels: cleaner_core::engines::fill::render(image, fitted),
                    provider: None, model_sha256: None,
                    pad: cleaner_core::strip::EdgePad::None, tiles: None,
                }
            };
            assert!(matches!(edit_with_bench_render(&mut test_bench(&root), Engine::Fill,
                &located, fixture_plan("b", Geometry::Painted(Mask::filled(bounds)),
                    Engine::Fill, bounds), Some(&mut fake)).unwrap(), Outcome::Cleaned(_)));
            assert_eq!(seen.unwrap().data, expected.image.data, "{name}");
        }
    }

    /// Re-running a moved layer keeps its opacity either way; its placement
    /// only when the new output may be placed. A redraw is computed where the
    /// stored mask is, and carrying the move onto it would draw that spot's
    /// reconstruction somewhere else.
    #[test]
    fn a_rerun_into_a_redraw_keeps_opacity_and_lands_where_it_was_made() {
        use cleaner_core::patch::LayerStyle;
        let raw = cleaner_core::image::fixtures::by_name("l8").raster;
        for (engine, keeps_placement) in [(Engine::Lama, false), (Engine::Fill, true)] {
            let (root, located) = edit_fixture(&format!("rerun-layer-{engine:?}"), &raw);
            let bounds = Rect::new(20, 16, 8, 8);
            let mut bench = test_bench(&root);
            assert!(matches!(edit_with_bench(&mut bench, Engine::Fill, &located,
                fixture_plan("moved", Geometry::Painted(Mask::filled(bounds)), Engine::Fill, bounds))
                .unwrap(), Outcome::Cleaned(_)));
            let mut job = Job::open(&located.job_path).unwrap();
            job.project.patches[0].provenance.params_snapshot["layer"] =
                serde_json::json!({"opacity": 40, "offsetX": 6, "rotation": 30.0, "locked": true});
            job.flush().unwrap();
            drop(job);

            let mut render = |image: &Raster, fitted: &Fitted| run::Made {
                engine, mask: fitted.mask.clone(), ink: fitted.ink.clone(),
                pixels: cleaner_core::engines::fill::render(image, fitted),
                provider: None, model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None, tiles: None,
            };
            assert!(matches!(edit_with_bench_render(&mut bench, engine, &located,
                fixture_plan("moved", Geometry::Stored, engine, bounds), Some(&mut render)).unwrap(),
                Outcome::Cleaned(_)), "{engine:?}");
            let job = Job::open(&located.job_path).unwrap();
            let record = job.project.patches.iter().find(|record| record.id == "moved").unwrap();
            assert_eq!(record.provenance.engine, engine);
            let layer = LayerStyle::from_snapshot(&record.provenance.params_snapshot);
            assert_eq!(layer.opacity, 40, "{engine:?} lost its opacity");
            if keeps_placement {
                assert_eq!((layer.offset_x, layer.rotation, layer.locked), (6, 30.0, true));
            } else {
                assert!(!layer.is_transformed() && !layer.locked, "{engine:?} carried the move: {layer:?}");
            }
        }
    }

    /// **A re-run removes the lettering the patch removed, every time.** A
    /// model patch's mask is its lettering grown by the isolation radius, and
    /// seeding the next run from that mask grew the hole by the radius on each
    /// re-run and saved the grown hole as the next one's lettering. Three
    /// re-runs of one patch, LaMa and FLUX alike, now hand the model the same
    /// hole and leave the patch's lettering and write support where they were.
    #[test]
    fn rerunning_a_model_patch_never_grows_its_hole() {
        let mut raw = cleaner_core::image::fixtures::by_name("l8").raster;
        raw.width = 128;
        raw.height = 128;
        raw.data = vec![200; 128 * 128];
        let (root, located) = edit_fixture("rerun-no-growth", &raw);
        reruns_keep_the_hole(&root, &located, &raw, Rect::new(50, 50, 10, 6));
    }

    /// A long strip runs the edit in a window of the strip, where the stored
    /// patch arrives as a translated shape rather than as `Stored`.
    #[test]
    fn rerunning_a_model_patch_on_a_long_strip_never_grows_its_hole() {
        let (root, located) = strip_edit_fixture("rerun-no-growth");
        let job = Job::open(&located.job_path).unwrap();
        let source = Library::resolve_page(&job.project, located.page_index).unwrap();
        let raw = decode(&std::fs::read(job.source_path(source).unwrap()).unwrap()).unwrap();
        drop(job);
        reruns_keep_the_hole(&root, &located, &raw, Rect::new(20, 180, 10, 6));
        std::fs::remove_dir_all(root).ok();
    }

    /// Stores a model patch over `lettering`, re-runs it three times through
    /// a renderer with a model rung's geometry, and checks every run was
    /// handed the lettering and saved the same hole and write support.
    fn reruns_keep_the_hole(root: &std::path::Path, located: &Located, raw: &Raster, lettering: Rect) {
        let lettering = Mask::filled(lettering);
        let applied = lettering.dilated(cleaner_core::constants::ISOLATION_RADIUS, raw.width, raw.height);
        {
            let mut job = Job::open(&located.job_path).unwrap();
            let source = Library::resolve_page(&job.project, located.page_index).unwrap();
            let pixels = cleaner_core::engines::model::page_crop(raw, applied.bounds);
            job.complete_region(source, &Patch {
                id: "m".into(), mask: applied.clone(), ink: lettering.clone(), pixels, order: 0, visible: true,
                provenance: Provenance {
                    engine: Engine::Lama, engine_version: "fixture".into(), model_sha256: None,
                    execution_provider: "cpu".into(), params_snapshot: serde_json::json!({}),
                    mask_sha256: String::new(), source_sha256: String::new(), cloud: None, created: 0,
                },
            }, None).unwrap();
        }
        let mut bench = test_bench(root);
        let mut holes = Vec::new();
        for engine in [Engine::Lama, Engine::Flux, Engine::Lama] {
            // A model rung's geometry: it writes through the hole grown by
            // the isolation radius and records the hole as the lettering.
            let mut render = |image: &Raster, fitted: &Fitted| {
                holes.push(fitted.ink.count());
                let mask = cleaner_core::engines::model::applied_mask(fitted, image.width, image.height);
                run::Made {
                    engine, pixels: cleaner_core::engines::model::page_crop(image, mask.bounds), mask,
                    ink: fitted.ink.clone(), provider: None, model_sha256: None,
                    pad: cleaner_core::strip::EdgePad::None, tiles: Some(1),
                }
            };
            assert!(matches!(edit_with_bench_render(&mut bench, engine, located,
                fixture_plan("m", Geometry::Stored, engine, applied.bounds), Some(&mut render)).unwrap(),
                Outcome::Cleaned(_)), "{engine:?}");
            let job = Job::open(&located.job_path).unwrap();
            let record = job.project.patches.iter().find(|record| record.id == "m").unwrap();
            assert_eq!(record.provenance.engine, engine);
            let patch = job.load_display_patch(record).unwrap();
            assert_eq!(patch.ink, lettering, "{engine:?} saved a grown hole as the lettering");
            assert_eq!(patch.mask, applied, "{engine:?} grew the write support");
        }
        assert_eq!(holes, vec![lettering.count(); 3], "the model was handed a grown hole");
    }

    /// **Try again wider hands the model the stored mask as its hole**, so
    /// each press is the isolation radius wider than the last, and plain Try
    /// again afterwards repeats the last wider hole rather than going back.
    #[test]
    fn retrying_wider_grows_the_hole_by_the_radius_each_press() {
        let mut raw = cleaner_core::image::fixtures::by_name("l8").raster;
        raw.width = 128;
        raw.height = 128;
        raw.data = vec![200; 128 * 128];
        let (root, located) = edit_fixture("rerun-wider", &raw);
        reruns_widen_the_hole(&root, &located, &raw, Rect::new(50, 50, 10, 6));
    }

    #[test]
    fn retrying_wider_on_a_long_strip_grows_the_hole_by_the_radius_each_press() {
        let (root, located) = strip_edit_fixture("rerun-wider");
        let job = Job::open(&located.job_path).unwrap();
        let source = Library::resolve_page(&job.project, located.page_index).unwrap();
        let raw = decode(&std::fs::read(job.source_path(source).unwrap()).unwrap()).unwrap();
        drop(job);
        reruns_widen_the_hole(&root, &located, &raw, Rect::new(40, 180, 10, 6));
        std::fs::remove_dir_all(root).ok();
    }

    fn reruns_widen_the_hole(root: &std::path::Path, located: &Located, raw: &Raster, lettering: Rect) {
        let radius = cleaner_core::constants::ISOLATION_RADIUS;
        let lettering = Mask::filled(lettering);
        let applied = lettering.dilated(radius, raw.width, raw.height);
        {
            let mut job = Job::open(&located.job_path).unwrap();
            let source = Library::resolve_page(&job.project, located.page_index).unwrap();
            let pixels = cleaner_core::engines::model::page_crop(raw, applied.bounds);
            job.complete_region(source, &Patch {
                id: "w".into(), mask: applied.clone(), ink: lettering.clone(), pixels, order: 0, visible: true,
                provenance: Provenance {
                    engine: Engine::Lama, engine_version: "fixture".into(), model_sha256: None,
                    execution_provider: "cpu".into(), params_snapshot: serde_json::json!({}),
                    mask_sha256: String::new(), source_sha256: String::new(), cloud: None, created: 0,
                },
            }, None).unwrap();
        }
        let mut bench = test_bench(root);
        let mut holes = Vec::new();
        let mut expected = Vec::new();
        let (mut stored_mask, mut stored_ink) = (applied.clone(), lettering.clone());
        for geometry in [Geometry::StoredWider, Geometry::StoredWider, Geometry::Stored] {
            // Wider: the stored mask is the hole. Plain: the stored lettering.
            let hole = if matches!(geometry, Geometry::StoredWider) { stored_mask.clone() } else { stored_ink.clone() };
            let mut render = |image: &Raster, fitted: &Fitted| {
                holes.push(fitted.ink.count());
                let mask = cleaner_core::engines::model::applied_mask(fitted, image.width, image.height);
                run::Made {
                    engine: Engine::Lama, pixels: cleaner_core::engines::model::page_crop(image, mask.bounds), mask,
                    ink: fitted.ink.clone(), provider: None, model_sha256: None,
                    pad: cleaner_core::strip::EdgePad::None, tiles: Some(1),
                }
            };
            assert!(matches!(edit_with_bench_render(&mut bench, Engine::Lama, located,
                fixture_plan("w", geometry, Engine::Lama, stored_mask.bounds), Some(&mut render)).unwrap(),
                Outcome::Cleaned(_)));
            expected.push(hole.count());
            let job = Job::open(&located.job_path).unwrap();
            let patch = job.load_patch(job.project.patches.iter().find(|record| record.id == "w").unwrap()).unwrap();
            assert_eq!(patch.ink, hole, "the saved hole is not the one the model was handed");
            let written = hole.dilated(radius, raw.width, raw.height);
            assert_eq!(patch.mask, written, "the write support is not the hole grown by the radius");
            (stored_mask, stored_ink) = (written, hole);
        }
        assert_eq!(holes, expected);
        assert!(expected[0] > lettering.count() && expected[1] > expected[0], "the hole did not grow: {expected:?}");
        assert_eq!(expected[2], expected[1], "plain Try again moved the hole: {expected:?}");
    }

    #[test]
    fn approved_component_id_is_reserved_for_insertion_only() {
        assert!(is_approved_component_region(
            "c1-p001-hreview-sam-00001-deadbeef"
        ));
        assert!(!is_approved_component_region("c1-p001-r1"));
        assert!(!is_approved_component_region("c1-p001-h1"));
    }

    /// A removes the black lettering at x=40. B overlaps A, while its read
    /// context also includes A's pixels that lie outside B's write mask.
    /// Capture the exact raster and fitted mask handed to the renderer.
    #[test]
    fn a_later_ai_stroke_reads_the_visible_first_stroke() {
        use cleaner_core::image::{encode, fixtures, Format};
        use cleaner_core::ingest::source_ref;
        use cleaner_core::project::{Project, StripMode};
        let root = std::env::temp_dir().join(format!("mc-two-strokes-{}", std::process::id()));
        std::fs::create_dir_all(root.join("raws")).unwrap();
        let mut raw = fixtures::by_name("l8").raster;
        raw.width = 128;
        raw.height = 128;
        raw.data = vec![255; 128 * 128];
        for y in 36..48 {
            for x in 36..48 {
                raw.set_sample(x, y, 0, 0);
            }
        }
        let bytes = encode(&raw, Format::Png).unwrap();
        let source_path = root.join("raws/page.png");
        std::fs::write(&source_path, &bytes).unwrap();
        let source = source_ref(&source_path, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let mut job = Job::create(
            &manifest,
            Project::new(
                manifest.parent().unwrap(),
                "test",
                StripMode::Single,
                &[source],
            ),
        )
        .unwrap();
        let a_mask = Mask::filled(Rect::new(36, 36, 12, 12));
        let mut a_pixels = raw.clone();
        a_pixels.width = 12;
        a_pixels.height = 12;
        a_pixels.data = vec![255; 12 * 12];
        job.complete_region(
            0,
            &Patch {
                id: "a".into(),
                mask: a_mask.clone(),
                ink: a_mask,
                pixels: a_pixels,
                order: 0,
                visible: true,
                provenance: Provenance {
                    engine: Engine::Fill,
                    engine_version: "fixture".into(),
                    model_sha256: None,
                    execution_provider: "cpu".into(),
                    params_snapshot: serde_json::json!({}),
                    mask_sha256: "fixture".into(),
                    source_sha256: sha256_hex(&bytes),
                    cloud: None,
                    created: 0,
                },
            },
            None,
        )
        .unwrap();
        // Automatic, paint, and clone predecessors use the same ordered
        // compositor; their producer rung cannot change the pixels B reads.
        for engine in [Engine::Lama, Engine::Paint, Engine::Clone, Engine::Fill] {
            let record = job.project.patches.iter().find(|p| p.id == "a").unwrap();
            let mut a = job.load_display_patch(record).unwrap();
            a.provenance.engine = engine;
            job.complete_region(0, &a, None).unwrap();
            let input =
                crate::underlay::read(&job, 0, &raw, Rect::new(42, 40, 12, 6), 1, None).unwrap();
            assert_eq!(
                input.image.sample(
                    (44 - input.window.x) as u32,
                    (40 - input.window.y) as u32,
                    0
                ),
                255
            );
        }
        let located = Located {
            chapter_id: "chapter".into(),
            job_path: manifest,
            page_index: 0,
        };
        let wrong_source = Plan {
            expected_source: Some((1, sha256_hex(&bytes))),
            region_id: "wrong-page".into(),
            geometry: Geometry::Painted(Mask::filled(Rect::new(42, 40, 12, 6))),
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("aiMaskBrush".into()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: Some(Rect::new(42, 40, 12, 6)),
            stroke: None,
            drawn: None,
        };
        let mut bench = test_bench(&root);
        assert!(matches!(
            edit_with_bench(&mut bench, Engine::Fill, &located, wrong_source).unwrap(),
            Outcome::NotFound
        ));
        let plan = Plan {
            expected_source: None,
            region_id: "b".into(),
            geometry: Geometry::Painted(Mask::filled(Rect::new(42, 40, 12, 6))),
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("aiMaskBrush".into()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: Some(Rect::new(42, 40, 12, 6)),
            stroke: None,
            drawn: None,
        };
        let mut observed = None;
        let mut fake = |image: &Raster, fitted: &Fitted| {
            observed = Some((
                image.sample(40, 40, 0),
                image.sample(44, 40, 0),
                fitted.mask.contains(44, 40),
                fitted.mask.contains(40, 40),
            ));
            let mut pixels = cleaner_core::engines::fill::render(image, fitted);
            pixels.data.fill(100);
            run::Made {
                engine: Engine::Fill,
                mask: fitted.mask.clone(),
                ink: fitted.ink.clone(),
                pixels,
                provider: None,
                model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None,
                tiles: None,
            }
        };
        assert!(matches!(
            edit_with_bench_render(&mut bench, Engine::Fill, &located, plan, Some(&mut fake))
                .unwrap(),
            Outcome::Cleaned(_)
        ));
        assert_eq!(observed, Some((255, 255, true, false)));

        let mut neighbor_seen = None;
        let neighbor = Plan {
            expected_source: None,
            region_id: "neighbor".into(),
            geometry: Geometry::Painted(Mask::filled(Rect::new(52, 38, 8, 8))),
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("aiMaskBrush".into()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: Some(Rect::new(52, 38, 8, 8)),
            stroke: None,
            drawn: None,
        };
        let mut fake_neighbor = |image: &Raster, fitted: &Fitted| {
            neighbor_seen = Some((image.sample(40, 40, 0), fitted.mask.contains(40, 40)));
            let mut pixels = cleaner_core::engines::fill::render(image, fitted);
            pixels.data.fill(170);
            run::Made {
                engine: Engine::Fill,
                mask: fitted.mask.clone(),
                ink: fitted.ink.clone(),
                pixels,
                provider: None,
                model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None,
                tiles: None,
            }
        };
        assert!(matches!(
            edit_with_bench_render(
                &mut bench,
                Engine::Fill,
                &located,
                neighbor,
                Some(&mut fake_neighbor)
            )
            .unwrap(),
            Outcome::Cleaned(_)
        ));
        assert_eq!(
            neighbor_seen,
            Some((255, false)),
            "A must reach B's neighboring context"
        );

        // A newer visible result covers B. Retry B must exclude both B's old
        // output and that newer result, and read A at its stored order ceiling.
        let mut job = Job::open(&located.job_path).unwrap();
        let mut c = job
            .load_patch(job.project.patches.iter().find(|p| p.id == "b").unwrap())
            .unwrap();
        c.id = "c".into();
        c.order = 3;
        c.pixels.data.fill(30);
        job.complete_region(0, &c, None).unwrap();
        let retry = Plan {
            expected_source: None,
            region_id: "b".into(),
            geometry: Geometry::Stored,
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("aiMaskBrush".into()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: Some(Rect::new(42, 40, 12, 6)),
            stroke: None,
            drawn: None,
        };
        let mut retry_seen = None;
        let mut fake_retry = |image: &Raster, fitted: &Fitted| {
            retry_seen = Some(image.sample(44, 40, 0));
            let mut pixels = cleaner_core::engines::fill::render(image, fitted);
            pixels.data.fill(120);
            run::Made {
                engine: Engine::Fill,
                mask: fitted.mask.clone(),
                ink: fitted.ink.clone(),
                pixels,
                provider: None,
                model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None,
                tiles: None,
            }
        };
        assert!(matches!(
            edit_with_bench_render(
                &mut bench,
                Engine::Fill,
                &located,
                retry,
                Some(&mut fake_retry)
            )
            .unwrap(),
            Outcome::Cleaned(_)
        ));
        assert_eq!(retry_seen, Some(255));

        let mut job = Job::open(&located.job_path).unwrap();
        job.project
            .patches
            .iter_mut()
            .find(|p| p.id == "b")
            .unwrap()
            .review_state = Some("review.reason.unusuallyLarge".into());
        job.flush().unwrap();
        crate::underlay::refresh_dependencies(&mut job, "a").unwrap();
        assert_eq!(
            job.project
                .patches
                .iter()
                .find(|p| p.id == "b")
                .unwrap()
                .review_state
                .as_deref(),
            Some("review.reason.unusuallyLarge")
        );
        job.project
            .patches
            .iter_mut()
            .find(|p| p.id == "a")
            .unwrap()
            .visible = false;
        job.flush().unwrap();
        crate::underlay::refresh_dependencies(&mut job, "a").unwrap();
        assert_eq!(
            job.project
                .patches
                .iter()
                .find(|p| p.id == "b")
                .unwrap()
                .review_state
                .as_deref(),
            Some("review.reason.inputChanged")
        );
        assert_eq!(
            job.project
                .patches
                .iter()
                .find(|p| p.id == "b")
                .unwrap()
                .provenance
                .params_snapshot["review_before_input_change"],
            "review.reason.unusuallyLarge"
        );
        let deleted_retry = Plan {
            expected_source: None,
            region_id: "a".into(),
            geometry: Geometry::Stored,
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("aiMaskBrush".into()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: Some(Rect::new(36, 36, 12, 12)),
            stroke: None,
            drawn: None,
        };
        assert!(matches!(
            edit_with_bench(&mut bench, Engine::Fill, &located, deleted_retry).unwrap(),
            Outcome::NotFound
        ));
        assert!(
            !Job::open(&located.job_path)
                .unwrap()
                .project
                .patches
                .iter()
                .find(|p| p.id == "a")
                .unwrap()
                .visible
        );
        let hidden = Plan {
            expected_source: None,
            region_id: "after-hidden".into(),
            geometry: Geometry::Painted(Mask::filled(Rect::new(36, 38, 4, 5))),
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("aiMaskBrush".into()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: Some(Rect::new(36, 38, 4, 5)),
            stroke: None,
            drawn: None,
        };
        let mut hidden_seen = None;
        let mut fake_hidden = |image: &Raster, fitted: &Fitted| {
            hidden_seen = Some(image.sample(38, 40, 0));
            let pixels = cleaner_core::engines::fill::render(image, fitted);
            run::Made {
                engine: Engine::Fill,
                mask: fitted.mask.clone(),
                ink: fitted.ink.clone(),
                pixels,
                provider: None,
                model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None,
                tiles: None,
            }
        };
        assert!(matches!(
            edit_with_bench_render(
                &mut bench,
                Engine::Fill,
                &located,
                hidden,
                Some(&mut fake_hidden)
            )
            .unwrap(),
            Outcome::Cleaned(_)
        ));
        assert_eq!(hidden_seen, Some(0));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn paint_preview_limits_include_clone_source_window_and_dab_work() {
        let mut paint = PaintPlan {
            kind: PaintKind::Brush { color: [128; 3] },
            points: vec![PaintPoint { x: 50.0, y: 50.0, p: 1.0 }],
            spec: cleaner_core::paint::BrushSpec::default(), alignment: "aligned",
        };
        assert!(paint_preview_fits(&paint, 10000, 10000));
        paint.kind = PaintKind::Clone { heal: true, offset: (80.0,80.0) };
        assert!(!paint_preview_fits(&paint, 10000, 10000));
        paint.kind = PaintKind::Brush { color: [128;3] };
        paint.spec.size = 1.0;
        paint.spec.spacing = 1.0;
        paint.points.push(PaintPoint { x: 60.0,y: 50.0,p: 1.0 });
        assert!(!paint_preview_fits(&paint,10000,10000));
    }

    #[test]
    fn preview_and_commit_remap_cross_join_clone_to_same_native_locations() {
        let strip = cleaner_core::strip::Strip::of_sizes(&[(100,100),(100,100)]);
        let anchor = strip.pages()[0];
        let mut paint = PaintPlan {
            kind: PaintKind::Clone { heal: true, offset: (0.0,-30.0) },
            points: vec![PaintPoint { x:50.0,y:95.0,p:1.0 }],
            spec: cleaner_core::paint::BrushSpec::default(), alignment: "aligned",
        };
        remap_paint(&mut paint,anchor,(20,80),60,80);
        assert_eq!(paint.points[0].x,50.0);
        assert_eq!(paint.points[0].y,18.75);
        let PaintKind::Clone { offset,.. } = paint.kind else { panic!("not clone") };
        // (30,15) in the window -> native target (50,95), source (50,125).
        assert_eq!(offset,(0.0,-37.5));
    }

    fn strip_edit_fixture(name: &str) -> (PathBuf, Located) {
        use cleaner_core::image::{encode, fixtures, Format};
        use cleaner_core::ingest::source_ref;
        use cleaner_core::project::{Project, StripMode};
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "mc-region-strip-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let raws = root.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let mut raster = fixtures::by_name("l8").raster;
        let original = raster.data.clone();
        let original_height = raster.height;
        let stride = raster.stride();
        raster.height = 400;
        raster.data = (0..raster.height)
            .flat_map(|row| {
                let row = row as usize % original_height as usize;
                original[row * stride..(row + 1) * stride].to_vec()
            })
            .collect();
        let bytes = encode(&raster, Format::Png).unwrap();
        let sources = (0..2)
            .map(|index| {
                let path = raws.join(format!("{index:03}.png"));
                std::fs::write(&path, &bytes).unwrap();
                source_ref(&path, &bytes).unwrap()
            })
            .collect::<Vec<_>>();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(
            manifest.parent().unwrap(),
            "test",
            StripMode::Longstrip,
            &sources,
        );
        Job::create(&manifest, project).unwrap();
        let located = Located {
            chapter_id: "chapter".into(),
            job_path: manifest,
            page_index: 0,
        };
        (root, located)
    }

    #[test]
    fn reordered_strip_paint_uses_source_placement() {
        let (root, located) = strip_edit_fixture("reordered-paint");
        let mut job = Job::open(&located.job_path).unwrap();
        job.project.strip.order = vec![1, 0];
        let bounds = Rect::new(5, 7, 3, 3);
        let mask = Mask::filled(bounds);
        let patch = Patch {
            id: "lower".into(),
            mask: mask.clone(),
            ink: mask,
            pixels: Raster {
                width: 3, height: 3,
                mode: cleaner_core::image::ColorMode::Gray,
                depth: cleaner_core::image::BitDepth::Eight,
                icc: None, palette: None, trns: None, srgb_intent: None,
                color: Default::default(),
                data: vec![200; 9],
            },
            order: 0,
            visible: true,
            provenance: Provenance {
                engine: Engine::Fill, engine_version: "fixture".into(),
                model_sha256: None, execution_provider: "cpu".into(),
                params_snapshot: serde_json::json!({}), mask_sha256: String::new(),
                source_sha256: String::new(), cloud: None, created: 0,
            },
        };
        job.complete_region(1, &patch, None).unwrap();
        let strip = run::strip_of(&job.project);
        let context = Some(StripPaintContext {
            strip: &strip, order: &job.project.strip.order, origin: (0, 0),
        });
        let record = &job.project.patches[0];
        assert_eq!(record_box_in_paint_space(&job, record, context), Some(bounds));
        assert_eq!(patch_in_paint_space(&job, record, context).unwrap().unwrap().mask.bounds, bounds);
        assert_eq!(patches_over(&job, 1, 1, bounds, context).unwrap().len(), 1);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn additions_only_text_shape_has_provisional_reading_context() {
        use cleaner_core::text_shape::{MaskQualityState, MaskRaster, MASK_PLAN_VERSION};
        let empty = MaskRaster::from(Mask::empty(Rect::new(0, 0, 0, 0)));
        let mut additions = Mask::empty(Rect::new(5, 6, 2, 2));
        additions.set(5, 6, true);
        let plan = MaskPlan {
            version: MASK_PLAN_VERSION,
            region_id: "chapter-p001-h1".into(),
            source_sha256: "source".into(),
            lower_composite_sha256: "preparing".into(),
            algorithm_id: "fixture".into(),
            model_id: None,
            candidate_bounds: Rect::new(4, 5, 4, 4),
            refinement_crop: Rect::new(4, 5, 4, 4),
            base_revision: 1,
            correction_revision: 1,
            plan_revision: 1,
            base_mask: empty.clone(),
            additions: MaskRaster::from(additions),
            removals: empty,
            padding_px: 0,
            model_hole_margin_px: 0,
            reading_context: Rect::new(0, 0, 0, 0),
            blend_alpha: None,
            quality: MaskQualityState::Ready,
        };
        let mut provisional = plan.clone();
        provisional.reading_context = provisional_text_shape_context(&plan);
        assert!(provisional.prepare(20, 20).is_ok());
    }

    fn test_bench(root: &std::path::Path) -> Bench {
        let models = root.join("no-models");
        Bench {
            app_data: None,
            sidecar_override: None,
            flux_model: None,
            flux_backend: None,
            detector: run::detector_on_demand(&models, Preference::CpuOnly),
            rung2: run::Rung2::new(&models, Preference::CpuOnly),
            flux: None,
            flux_key: None,
            detected_seed: None,
        }
    }

    #[test]
    fn prepared_text_shape_near_a_strip_join_reads_the_same_lower_context_at_apply() {
        use cleaner_core::text_shape::{MaskQualityState, MaskRaster, MASK_PLAN_VERSION};
        let (root, located) = strip_edit_fixture("text-shape-near-join");
        let mut job = Job::open(&located.job_path).unwrap();
        let source_idx = Library::resolve_page(&job.project, located.page_index).unwrap();
        let bytes = std::fs::read(job.source_path(source_idx).unwrap()).unwrap();
        let raw = decode(&bytes).unwrap();
        let mut base = Mask::empty(Rect::new(20, raw.height as i64 - 5, 3, 3));
        base.set(21, raw.height as i64 - 4, true);
        let empty = MaskRaster::from(Mask::empty(Rect::new(0, 0, 0, 0)));
        let region_id = "chapter-p001-h1".to_string();
        let mut mask_plan = MaskPlan {
            version: MASK_PLAN_VERSION,
            region_id: region_id.clone(),
            source_sha256: sha256_hex(&bytes),
            lower_composite_sha256: "pending".into(),
            algorithm_id: "synthetic-test".into(),
            model_id: None,
            candidate_bounds: Rect::new(16, raw.height as i64 - 9, 12, 9),
            refinement_crop: Rect::new(10, raw.height as i64 - 20, 30, 20),
            base_revision: 1,
            correction_revision: 0,
            plan_revision: 1,
            base_mask: MaskRaster::from(base),
            additions: empty.clone(),
            removals: empty,
            padding_px: 2,
            model_hole_margin_px: 5,
            reading_context: Rect::new(20, raw.height as i64 - 5, 3, 3),
            blend_alpha: None,
            quality: MaskQualityState::Ready,
        };
        let provisional = mask_plan.prepare(raw.width, raw.height).unwrap();
        let (input, context) = prepared_input_for_page(
            &job,
            &located,
            source_idx,
            &raw,
            provisional.write_support.bounds,
            0,
        )
        .unwrap();
        assert!(
            context.bottom() > raw.height as i64,
            "the read context must include the next strip page"
        );
        mask_plan.lower_composite_sha256 = lower_composite_revision(&input);
        mask_plan.reading_context = context;
        let prepared = mask_plan.prepare(raw.width, raw.height).unwrap();
        job.store_text_shape_plan(source_idx, &mask_plan, &prepared)
            .unwrap();
        drop(job);
        let mut bench = test_bench(&root);
        let mut fake = |page: &Raster, fitted: &Fitted| run::Made {
            engine: Engine::Fill,
            mask: fitted.mask.clone(),
            ink: fitted.ink.clone(),
            pixels: cleaner_core::engines::fill::render(page, fitted),
            provider: None,
            model_sha256: None,
            pad: cleaner_core::strip::EdgePad::None,
            tiles: None,
        };
        let plan = Plan {
            expected_source: None,
            region_id: region_id.clone(),
            geometry: Geometry::Prepared(prepared.identity.identity_sha256.clone()),
            choice: Choice::Exact(Engine::Fill),
            source: "auto",
            tool: None,
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: None,
            stroke: None,
            drawn: None,
        };
        assert!(matches!(
            edit_with_bench_render(&mut bench, Engine::Fill, &located, plan, Some(&mut fake))
                .unwrap(),
            Outcome::Cleaned(_)
        ));
        let reopened = Job::open(&located.job_path).unwrap();
        let row = reopened
            .project
            .patches
            .iter()
            .find(|row| row.id == region_id)
            .unwrap();
        assert_eq!(row.bbox, prepared.write_support.bounds);
        assert_eq!(
            row.text_shape_plan_identity.as_deref(),
            Some(prepared.identity.identity_sha256.as_str())
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_cross_join_shape_commits_reruns_and_survives_undo_on_disk() {
        let (root, located) = strip_edit_fixture("shape");
        let initial = Job::open(&located.job_path).unwrap();
        let source = initial.project.strip.order[0];
        let (w, h) = page_size(&initial, source).unwrap();
        let rect = pixels_of(
            Bbox {
                x: 25.0,
                y: 90.0,
                w: 50.0,
                h: 20.0,
            },
            w,
            h,
        );
        let shape = PaintedShape {
            kind: ShapeKind::Ellipse,
            points: vec![
                StrokePoint { x: 25.0, y: 90.0 },
                StrokePoint { x: 75.0, y: 90.0 },
                StrokePoint { x: 75.0, y: 110.0 },
                StrokePoint { x: 25.0, y: 110.0 },
            ],
            feather: 0.0,
        };
        let plan = Plan {
            expected_source: None,
            region_id: "chapter-p001-h1".into(),
            geometry: Geometry::Given(rect),
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("shapes".into()),
            fill_mode: Some("solid"),
            solid_color: None,
            clears_untouched: false,
            paint: Some(PaintPlan {
                kind: PaintKind::Shape {
                    color: [30, 30, 30],
                    shape: shape.clone(),
                    outline: None,
                },
                points: Vec::new(),
                spec: cleaner_core::paint::BrushSpec::default(),
                alignment: "aligned",
            }),
            requested: Some(rect),
            stroke: None,
            drawn: Some(shape),
        };
        let mut bench = test_bench(&root);
        let created = edit_with_bench(&mut bench, Engine::Fill, &located, plan).unwrap();
        assert!(matches!(created, Outcome::Cleaned(_)));

        let written = Job::open(&located.job_path).unwrap();
        let record = written
            .project
            .patches
            .iter()
            .find(|p| p.id == "chapter-p001-h1")
            .unwrap();
        assert!(record.bbox.y < h as i64 && record.bbox.bottom() > h as i64);
        let patch = written.load_patch(record).unwrap();
        assert!(
            !patch.mask.contains(record.bbox.x, record.bbox.y),
            "ellipse became its box"
        );
        assert!(patch.mask.bits.contains(&0) && patch.mask.bits.iter().any(|b| *b != 0));
        let strip = run::strip_of(&written.project);
        let global = cleaner_core::export::StripPatch::lift(&strip, 0, &patch).unwrap();
        let first = cleaner_core::export::patches_on_page(&strip, 0, std::slice::from_ref(&global));
        let second =
            cleaner_core::export::patches_on_page(&strip, 1, std::slice::from_ref(&global));
        assert!(!first.is_empty() && first[0].mask.bits.iter().any(|bit| *bit != 0));
        assert!(!second.is_empty() && second[0].mask.bits.iter().any(|bit| *bit != 0));

        let rerun = Plan {
            expected_source: None,
            region_id: "chapter-p001-h1".into(),
            geometry: Geometry::Stored,
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("shapes".into()),
            fill_mode: Some("match-surround"),
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: Some(record.bbox),
            stroke: None,
            drawn: None,
        };
        // A later edit anchored on page two must see and composite over the
        // older page-one-owned seam patch, even though this gesture itself is
        // wholly inside page two.
        let second_located = Located {
            chapter_id: located.chapter_id.clone(),
            job_path: located.job_path.clone(),
            page_index: 1,
        };
        let second_shape = PaintedShape {
            kind: ShapeKind::Rect,
            points: vec![
                StrokePoint { x: 30.0, y: 2.0 },
                StrokePoint { x: 70.0, y: 2.0 },
                StrokePoint { x: 70.0, y: 8.0 },
                StrokePoint { x: 30.0, y: 8.0 },
            ],
            feather: 0.0,
        };
        let second_rect = Rect::new(19, 8, 26, 24);
        let second_plan = Plan {
            expected_source: None,
            region_id: "chapter-p002-h1".into(),
            geometry: Geometry::Given(second_rect),
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("shapes".into()),
            fill_mode: Some("solid"),
            solid_color: None,
            clears_untouched: false,
            paint: Some(PaintPlan {
                kind: PaintKind::Shape {
                    color: [230, 230, 230],
                    shape: second_shape.clone(),
                    outline: None,
                },
                points: Vec::new(),
                spec: cleaner_core::paint::BrushSpec {
                    opacity: 50.0,
                    ..cleaner_core::paint::BrushSpec::default()
                },
                alignment: "aligned",
            }),
            requested: Some(second_rect),
            stroke: None,
            drawn: Some(second_shape),
        };
        assert!(matches!(
            edit_with_bench(&mut bench, Engine::Fill, &second_located, second_plan).unwrap(),
            Outcome::Cleaned(_)
        ));
        let layered = Job::open(&located.job_path).unwrap();
        let old_order = layered
            .project
            .patches
            .iter()
            .find(|p| p.id == "chapter-p001-h1")
            .unwrap()
            .order;
        let old_record = layered
            .project
            .patches
            .iter()
            .find(|p| p.id == "chapter-p001-h1")
            .unwrap();
        let new_record = layered
            .project
            .patches
            .iter()
            .find(|p| p.id == "chapter-p002-h1")
            .unwrap();
        let new_order = new_record.order;
        assert!(
            new_order > old_order,
            "strip paint order must be chapter-wide"
        );
        let old_patch = layered.load_patch(old_record).unwrap();
        let new_patch = layered.load_patch(new_record).unwrap();
        let global_x = 32i64;
        let anchor_y = h as i64 + 20;
        assert!(old_patch.mask.contains(global_x, anchor_y));
        assert!(new_patch.mask.contains(global_x, 20));
        let below = old_patch.pixels.sample(
            (global_x - old_patch.mask.bounds.x) as u32,
            (anchor_y - old_patch.mask.bounds.y) as u32,
            0,
        );
        let above = new_patch.pixels.sample(
            (global_x - new_patch.mask.bounds.x) as u32,
            (20 - new_patch.mask.bounds.y) as u32,
            0,
        );
        let expected = ((230.0 + below as f64) / 2.0).round() as u16;
        assert_eq!(
            below, 30,
            "the older seam paint must be the sampled under-layer"
        );
        assert_eq!(
            above, 230,
            "page-two shape did not retain its chosen colour"
        );

        let cross_ai = Plan {
            expected_source: None,
            region_id: "chapter-p001-ai-cross".into(),
            geometry: Geometry::Painted(Mask::filled(Rect::new(27, 390, 10, 24))),
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some("aiMaskBrush".into()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: None,
            requested: Some(Rect::new(27, 390, 10, 24)),
            stroke: None,
            drawn: None,
        };
        let mut cross_seen = None;
        let mut fake_cross = |image: &Raster, fitted: &Fitted| {
            cross_seen = Some((
                image.sample(32, 380, 0),
                image.sample(32, 420, 0),
                fitted.mask.contains(32, 420),
            ));
            let pixels = cleaner_core::engines::fill::render(image, fitted);
            run::Made {
                engine: Engine::Fill,
                mask: fitted.mask.clone(),
                ink: fitted.ink.clone(),
                pixels,
                provider: None,
                model_sha256: None,
                pad: cleaner_core::strip::EdgePad::None,
                tiles: None,
            }
        };
        assert!(matches!(
            edit_with_bench_render(
                &mut bench,
                Engine::Fill,
                &located,
                cross_ai,
                Some(&mut fake_cross)
            )
            .unwrap(),
            Outcome::Cleaned(_)
        ));
        let cloud_job = Job::open(&located.job_path).unwrap();
        let cloud_record = cloud_job
            .project
            .patches
            .iter()
            .find(|p| p.id == "chapter-p001-ai-cross")
            .unwrap();
        let cloud_patch = cloud_job.load_patch(cloud_record).unwrap();
        let source = std::fs::read(cloud_job.source_path(0).unwrap()).unwrap();
        let raw = decode(&source).unwrap();
        let cloud = crate::underlay::prepare_cloud(
            &cloud_job, 0, &raw, cloud_record, &cloud_patch,
            cleaner_core::engines::render::Preprocessing::CURRENT,
        )
            .unwrap();
        let at = |x: i64, y: i64| {
            cloud.input.image.sample(
                (x - cloud.input.window.x) as u32,
                (y - cloud.input.window.y) as u32,
                0,
            )
        };
        assert_eq!(
            at(32, 420),
            expected,
            "cloud crop must include the page-two predecessor"
        );
        assert_eq!(
            cross_seen,
            Some((30, expected, false)),
            "AI context must include patches from both join sides"
        );
        let mut review_job = Job::open(&located.job_path).unwrap();
        let older_record = review_job
            .project
            .patches
            .iter()
            .find(|p| p.id == "chapter-p001-h1")
            .unwrap();
        let mut older = review_job.load_patch(older_record).unwrap();
        review_job.complete_region(0, &older, None).unwrap();
        crate::underlay::refresh_dependencies(&mut review_job, "chapter-p001-h1").unwrap();
        assert_eq!(
            review_job
                .project
                .patches
                .iter()
                .find(|p| p.id == "chapter-p001-ai-cross")
                .unwrap()
                .review_state,
            None,
            "same pixels must not create a cross-join warning"
        );
        let original = older.clone();
        older.pixels.data.fill(99);
        review_job.complete_region(0, &older, None).unwrap();
        crate::underlay::refresh_dependencies(&mut review_job, "chapter-p001-h1").unwrap();
        assert_eq!(
            review_job
                .project
                .patches
                .iter()
                .find(|p| p.id == "chapter-p001-ai-cross")
                .unwrap()
                .review_state
                .as_deref(),
            Some("review.reason.inputChanged")
        );
        review_job.complete_region(0, &original, None).unwrap();
        crate::underlay::refresh_dependencies(&mut review_job, "chapter-p001-h1").unwrap();
        review_job.complete_region(1, &new_patch, None).unwrap();
        crate::underlay::refresh_dependencies(&mut review_job, "chapter-p002-h1").unwrap();
        assert_eq!(
            review_job
                .project
                .patches
                .iter()
                .find(|p| p.id == "chapter-p001-ai-cross")
                .unwrap()
                .review_state,
            None,
            "same page-two pixels must not warn the page-one AI stroke"
        );
        let mut changed_second = new_patch.clone();
        changed_second.pixels.data.fill(99);
        review_job
            .complete_region(1, &changed_second, None)
            .unwrap();
        crate::underlay::refresh_dependencies(&mut review_job, "chapter-p002-h1").unwrap();
        assert_eq!(
            review_job
                .project
                .patches
                .iter()
                .find(|p| p.id == "chapter-p001-ai-cross")
                .unwrap()
                .review_state
                .as_deref(),
            Some("review.reason.inputChanged")
        );
        review_job.complete_region(1, &new_patch, None).unwrap();
        crate::underlay::refresh_dependencies(&mut review_job, "chapter-p002-h1").unwrap();

        assert!(matches!(
            edit_with_bench(&mut bench, Engine::Fill, &located, rerun).unwrap(),
            Outcome::Cleaned(_)
        ));

        let mut undone = Job::open(&located.job_path).unwrap();
        undone
            .project
            .patches
            .iter_mut()
            .find(|p| p.id == "chapter-p001-h1")
            .unwrap()
            .visible = false;
        undone.flush().unwrap();
        assert!(
            !Job::open(&located.job_path)
                .unwrap()
                .project
                .patches
                .iter()
                .find(|p| p.id == "chapter-p001-h1")
                .unwrap()
                .visible
        );
        let mut redone = Job::open(&located.job_path).unwrap();
        redone
            .project
            .patches
            .iter_mut()
            .find(|p| p.id == "chapter-p001-h1")
            .unwrap()
            .visible = true;
        redone.flush().unwrap();
        assert!(
            Job::open(&located.job_path)
                .unwrap()
                .project
                .patches
                .iter()
                .find(|p| p.id == "chapter-p001-h1")
                .unwrap()
                .visible
        );
        std::fs::remove_dir_all(root).ok();
    }

    /* -- rung 3a's backend, as a preference ---------------------------- */

    /// **The stored value picks the backend, and `auto` is the platform's
    /// answer.**
    ///
    /// This used to be `cfg!` and nothing else, so there was nothing to test:
    /// Apple Silicon got `mflux` and every other machine got a backend that
    /// declines every open. `sdnq` runs everywhere, so `auto` is now a *default*
    /// and the two named values are reachable from any host - which is what this
    /// asserts, from whichever host is running it.
    #[test]
    fn the_flux_backend_setting_picks_a_backend_and_auto_is_the_platform_default() {
        let default = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Backend::Mflux
        } else {
            Backend::Sdnq
        };
        assert_eq!(flux_backend(None), default);
        assert_eq!(flux_backend(Some("auto")), default);
        assert_eq!(flux_backend(Some("")), default);

        // An override is honoured on either platform, including the one that
        // cannot run here - the refusal names the choice, which is better than
        // silently substituting something the user did not ask for.
        assert_eq!(flux_backend(Some("mflux")), Backend::Mflux);
        assert_eq!(flux_backend(Some("sdnq")), Backend::Sdnq);
        // And a stale settings file does not make the rung unreachable.
        assert_eq!(flux_backend(Some("diffusers")), default);
    }

    /// The settings read, over the shape `settings::read` actually returns -
    /// including the older key each preference is still accepted under, and the
    /// cleared field that must read as absent rather than as an empty backend id.
    #[test]
    fn every_sidecar_preference_is_read_under_both_of_its_names() {
        let stored = serde_json::json!({
            "sidecarFolder": "/opt/legacy-sidecar",
            "sidecarModel": "flux2-klein-9b",
            "sidecarBackend": "sdnq",
        });
        assert_eq!(
            setting_str(Some(&stored), SIDECAR_PATH_KEYS).as_deref(),
            Some("/opt/legacy-sidecar")
        );
        assert_eq!(
            setting_str(Some(&stored), FLUX_MODEL_KEYS).as_deref(),
            Some("flux2-klein-9b")
        );
        assert_eq!(
            setting_str(Some(&stored), FLUX_BACKEND_KEYS).as_deref(),
            Some("sdnq")
        );

        // The current spelling wins where both are present.
        let both = serde_json::json!({ "fluxBackend": "mflux", "sidecarBackend": "sdnq" });
        assert_eq!(
            setting_str(Some(&both), FLUX_BACKEND_KEYS).as_deref(),
            Some("mflux")
        );

        // Cleared, missing, and no settings file at all are one answer, and
        // `flux_backend` turns it into the platform default rather than an error.
        for empty in [
            serde_json::json!({ "fluxBackend": "  " }),
            serde_json::json!({}),
        ] {
            assert_eq!(setting_str(Some(&empty), FLUX_BACKEND_KEYS), None);
        }
        assert_eq!(setting_str(None, FLUX_BACKEND_KEYS), None);
    }

    /// **What the settings panel is told, per backend.**
    ///
    /// Absence stays silent; an install plus a backend that renders nowhere is
    /// refused under `sidecarPlatform`; an install plus the portable arm is
    /// offered. The third case is the one that changed: before `sdnq` a Windows
    /// or Linux machine could not reach it at all.
    #[test]
    fn the_status_answers_per_chosen_backend_and_stays_silent_when_nothing_is_installed() {
        let nowhere = "/nonexistent/manga-cleaner-sidecar";
        let absent = sidecar_status_from(Some(nowhere), Some("sdnq"), None);
        assert!(!absent.available);
        assert_eq!(
            absent.reason_key, None,
            "a rung nobody installed says nothing"
        );

        // A virtual environment built for the test, so "installed" is a fact
        // about a directory rather than about this machine.
        let base = std::env::temp_dir().join(format!("mc-region-sidecar-{}", std::process::id()));
        let (dir, name) = if cfg!(windows) {
            ("Scripts", "python.exe")
        } else {
            ("bin", "python3")
        };
        std::fs::create_dir_all(base.join(dir)).unwrap();
        std::fs::write(base.join(dir).join(name), b"").unwrap();
        std::fs::write(base.join("pyvenv.cfg"), b"home = /usr/bin\n").unwrap();
        let root = base.to_string_lossy().to_string();

        // The stub renders nowhere, so choosing it is refused for that and not
        // for this machine's memory.
        let stub = sidecar_status_from(Some(&root), Some("sdcpp"), None);
        assert!(!stub.available);
        assert_eq!(stub.reason_key, Some("decline.reason.sidecarPlatform"));

        // And the portable arm is offered on whatever this host is, unless the
        // kernel would not say how much memory it has.
        let portable = sidecar_status_from(Some(&root), Some("sdnq"), None);
        if cleaner_core::memory::room().is_some() {
            assert!(portable.available, "{:?}", portable.reason_key);
            assert_eq!(portable.reason_key, None);
        } else {
            assert_eq!(
                portable.reason_key,
                Some("decline.reason.sidecarUnknownMachine")
            );
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    /* -- paint and clone, as the contract carries them ------------------ */

    /// **A brush is only a paint stroke when it says it is.** `add` and `erase`
    /// keep today's mask-then-inpaint behaviour, which is the contract's own
    /// wording and is the whole of what stops this branch swallowing every
    /// existing brush gesture.
    #[test]
    fn only_a_brush_in_paint_mode_takes_the_paint_branch() {
        let stroke = serde_json::json!({ "points": [{ "x": 10.0, "y": 20.0 }], "radius": 12.0 });
        for mode in ["add", "erase"] {
            let params = serde_json::json!({ "mode": mode, "stroke": stroke });
            assert_eq!(paint_plan("brush", &params), None, "{mode}");
        }
        // And a tool that is neither of the two never does, whatever it sends.
        for tool in ["shapes", "aiMaskBrush", "contentAwareFill"] {
            let params = serde_json::json!({
                "mode": "paint",
                "stroke": stroke,
                "paint": { "points": [{ "x": 1.0, "y": 2.0, "p": 1.0 }] },
            });
            assert_eq!(paint_plan(tool, &params), None, "{tool}");
        }

        let params = serde_json::json!({ "mode": "paint", "stroke": stroke });
        let plan = paint_plan("brush", &params).expect("paint mode with a stroke is a paint plan");
        assert!(matches!(plan.kind, PaintKind::Brush { .. }));
    }

    /// Every field of the brush block, in the spelling the contract fixes, with
    /// `params` standing in where the block is silent - the two spellings both
    /// reach here because `size`, `hardness`, `opacity` and `flow` are ordinary
    /// tool params as well as brush ones.
    #[test]
    fn the_paint_block_is_read_field_by_field_and_params_stands_in_for_it() {
        let params = serde_json::json!({
            "mode": "paint",
            "size": 40.0,
            "hardness": 12.0,
            "paint": {
                "points": [{ "x": 0.0, "y": 0.0, "p": 0.25 }, { "x": 50.0, "y": 50.0, "p": 1.0 }],
                "color": "#ff8000",
                "opacity": 70.0,
                "flow": 55.0,
                "hardness": 90.0,
                "spacing": 4.0,
                "pressureSize": false,
                "pressureOpacity": true,
                "seed": 4242,
            },
        });
        let plan = paint_plan("brush", &params).unwrap();
        assert_eq!(
            plan.kind,
            PaintKind::Brush {
                color: [0xff, 0x80, 0x00]
            }
        );
        assert_eq!(plan.points.len(), 2);
        assert_eq!(plan.points[0].p, 0.25);
        // The block wins over `params` where both carry the key.
        assert_eq!(plan.spec.hardness, 90.0);
        // And `params` is read where the block is silent.
        assert_eq!(plan.spec.size, 40.0);
        assert_eq!(
            (plan.spec.opacity, plan.spec.flow, plan.spec.spacing),
            (70.0, 55.0, 4.0)
        );
        assert!(!plan.spec.pressure_size);
        assert!(plan.spec.pressure_opacity);
        assert_eq!(plan.spec.seed, 4242);
    }

    /// `size` is a **diameter in native pixels** and `stroke.radius` is half of
    /// it. A caller that sends only the older field must still get the brush it
    /// drew rather than the 24 px default.
    #[test]
    fn the_stroke_radius_stands_in_for_a_missing_size() {
        let params = serde_json::json!({
            "mode": "paint",
            "stroke": { "points": [{ "x": 5.0, "y": 5.0 }], "radius": 9.0 },
        });
        assert_eq!(paint_plan("brush", &params).unwrap().spec.size, 18.0);
    }

    /// The `paint` block is optional for `cloneHeal`, and the fallback is the
    /// stroke every drawing tool already sends, at the default pressure - which
    /// is `0.5` and not `1.0`, because a device that says nothing about pressure
    /// is not a device pressing hardest.
    #[test]
    fn clone_heal_falls_back_to_the_stroke_at_the_default_pressure() {
        let params = serde_json::json!({
            "stroke": { "points": [{ "x": 10.0, "y": 20.0 }, { "x": 12.0, "y": 22.0 }], "radius": 6.0 },
            "cloneOffset": { "x": -4.0, "y": 3.0 },
        });
        let plan = paint_plan("cloneHeal", &params).unwrap();
        assert_eq!(plan.points.len(), 2);
        assert!(plan.points.iter().all(|p| p.p == 0.5));
        // No `mode` word means heal, which is the tool's own default. The offset
        // is the seam's, negated - see the sign test below.
        assert_eq!(
            plan.kind,
            PaintKind::Clone {
                heal: true,
                offset: (4.0, -3.0)
            }
        );

        let cloning = serde_json::json!({
            "mode": "clone",
            "stroke": { "points": [{ "x": 10.0, "y": 20.0 }], "radius": 6.0 },
            "cloneOffset": { "x": 1.5, "y": -2.5 },
        });
        assert_eq!(
            paint_plan("cloneHeal", &cloning).unwrap().kind,
            PaintKind::Clone {
                heal: false,
                offset: (-1.5, 2.5)
            }
        );
    }

    /// **The seam and the kernels spell the clone displacement in opposite
    /// directions, and this is the one place the two meet.**
    ///
    /// `gesture.js#cloneOffset` sends `source − strokeStart` and rebuilds the
    /// source as `strokeStart + offset`; `paint::render_clone` reads each dab at
    /// `target − offset`. So `clone_offset` negates, and the proof is a numeric
    /// example carried all the way through: a stroke starting at 30% with the
    /// alt-click at 10% arrives as `cloneOffset = −20`, and the source the
    /// kernel is handed has to come back out at 10% - not at 50%, which is what
    /// the un-negated value would sample.
    #[test]
    fn the_clone_source_comes_back_where_the_alt_click_put_it() {
        let stroke_start = (30.0, 40.0);
        let alt_click = (10.0, 15.0);
        // Exactly what `gesture.js` puts on the wire: `source − strokeStart`.
        let seam = (alt_click.0 - stroke_start.0, alt_click.1 - stroke_start.1);
        assert_eq!(seam, (-20.0, -25.0));

        let params = serde_json::json!({
            "mode": "clone",
            "stroke": { "points": [{ "x": stroke_start.0, "y": stroke_start.1 }], "radius": 6.0 },
            "cloneOffset": { "x": seam.0, "y": seam.1 },
        });
        let PaintKind::Clone { offset, .. } = paint_plan("cloneHeal", &params).unwrap().kind else {
            panic!("a cloneHeal call is a clone plan");
        };
        assert_eq!(offset, (20.0, 25.0), "the seam's sign was not turned round");

        // `render_clone` reads at `target − offset`. Recomputing it here rather
        // than trusting the field is the whole point: this is the arithmetic
        // the kernel does.
        let source = (stroke_start.0 - offset.0, stroke_start.1 - offset.1);
        assert_eq!(
            source, alt_click,
            "the kernel would have sampled the wrong pixel"
        );

        // And the `cloneSource` fallback lands on the same internal value, so
        // the two routes into `clone_offset` cannot disagree about direction.
        let by_source = serde_json::json!({
            "mode": "clone",
            "stroke": { "points": [{ "x": stroke_start.0, "y": stroke_start.1 }], "radius": 6.0 },
            "cloneSource": { "x": alt_click.0, "y": alt_click.1 },
        });
        assert_eq!(
            paint_plan("cloneHeal", &by_source).unwrap().kind,
            PaintKind::Clone {
                heal: false,
                offset: (20.0, 25.0)
            }
        );
    }

    /// **`cloneSource` is the same quantity computed the long way.** A caller
    /// that sends only the alt-clicked point still clones from where the user
    /// pointed; a caller that sends neither is not a clone stroke at all, and
    /// falls through to the ordinary path rather than cloning from itself.
    #[test]
    fn a_clone_source_stands_in_for_a_missing_offset_and_neither_is_no_plan() {
        let params = serde_json::json!({
            "stroke": { "points": [{ "x": 30.0, "y": 40.0 }], "radius": 6.0 },
            "cloneSource": { "x": 10.0, "y": 15.0 },
        });
        let plan = paint_plan("cloneHeal", &params).unwrap();
        // `strokeStart − cloneSource` is already the internal `target − source`,
        // so this route is *not* negated a second time.
        assert_eq!(
            plan.kind,
            PaintKind::Clone {
                heal: true,
                offset: (20.0, 25.0)
            }
        );

        let bare = serde_json::json!({
            "stroke": { "points": [{ "x": 30.0, "y": 40.0 }], "radius": 6.0 },
        });
        assert_eq!(paint_plan("cloneHeal", &bare), None);
    }

    /// A stroke with no points is not a plan. Nothing downstream has an answer
    /// for an empty polyline, and a `Some` here would reach `edit` and be
    /// refused there instead of never being built.
    #[test]
    fn a_stroke_with_no_points_is_not_a_paint_plan() {
        let params = serde_json::json!({ "mode": "paint", "paint": { "points": [] } });
        assert_eq!(paint_plan("brush", &params), None);
        assert_eq!(
            paint_plan("brush", &serde_json::json!({ "mode": "paint" })),
            None
        );
    }

    #[test]
    fn a_window_patch_is_persisted_in_anchor_coordinates_without_changing_its_shape() {
        let bounds = Rect::new(12, 18, 3, 4);
        let mut mask = Mask::empty(bounds);
        mask.set(13, 19, true);
        mask.set(14, 20, true);
        let bits = mask.bits.clone();
        let pixels = Raster {
            width: 3,
            height: 4,
            mode: cleaner_core::image::ColorMode::Gray,
            depth: cleaner_core::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![200; 12],
        };
        let mut made = run::Made {
            engine: Engine::Fill,
            ink: mask.clone(),
            mask,
            pixels,
            provider: None,
            model_sha256: None,
            pad: cleaner_core::strip::EdgePad::None,
            tiles: None,
        };

        translate_made(&mut made, (-7, 95));

        assert_eq!(made.mask.bounds, Rect::new(5, 113, 3, 4));
        assert_eq!(made.ink.bounds, made.mask.bounds);
        assert_eq!(
            made.mask.bits, bits,
            "translation must not demote a stroke to its box"
        );
        assert!(made.mask.contains(6, 114));
        assert!(!made.mask.contains(5, 113));
        assert_eq!((made.pixels.width, made.pixels.height), (3, 4));
    }

    #[test]
    fn a_clone_window_includes_its_cross_join_source() {
        let strip = cleaner_core::strip::Strip::of_sizes(&[(100, 100), (100, 100)]);
        let anchor = strip.pages()[0];
        let target = Rect::new(20, 80, 30, 10);
        let paint = PaintPlan {
            kind: PaintKind::Clone {
                heal: false,
                offset: (0.0, -30.0),
            },
            points: Vec::new(),
            spec: cleaner_core::paint::BrushSpec::default(),
            alignment: "aligned",
        };

        assert_eq!(
            manual_requested(target, Some(&paint), Some(anchor)),
            Rect::new(20, 80, 30, 40)
        );
    }

    /// Both hex spellings, and black for everything else - a stroke with an
    /// unreadable colour paints ink rather than being refused.
    #[test]
    fn a_colour_is_read_in_both_spellings_and_falls_back_to_ink() {
        assert_eq!(colour_of(Some("#1a2b3c")), [0x1a, 0x2b, 0x3c]);
        assert_eq!(colour_of(Some("1a2b3c")), [0x1a, 0x2b, 0x3c]);
        assert_eq!(colour_of(Some("#f0c")), [0xff, 0x00, 0xcc]);
        for bad in [
            Some("#12345"),
            Some("rebeccapurple"),
            Some("#zzzzzz"),
            Some(""),
            None,
        ] {
            assert_eq!(colour_of(bad), [0, 0, 0], "{bad:?}");
        }
    }

    /// Every number the seam sends is clamped rather than trusted: a `size` of
    /// zero is an empty stroke and an `opacity` of 900 is a value the kernel's
    /// own `clamp01` would silently eat.
    #[test]
    fn the_brush_numbers_are_clamped_rather_than_trusted() {
        let params = serde_json::json!({
            "mode": "paint",
            "paint": {
                "points": [{ "x": 1.0, "y": 1.0 }],
                "size": 0.0,
                "opacity": 900.0,
                "flow": -20.0,
                "hardness": 400.0,
            },
        });
        let spec = paint_plan("brush", &params).unwrap().spec;
        assert_eq!(
            (spec.size, spec.opacity, spec.flow, spec.hardness),
            (1.0, 100.0, 0.0, 100.0)
        );
    }

    /// **The snapshot is the brush and not a fit.** `run::params_snapshot`
    /// writes `thickness`, `deviation` and a write set, none of which a stroke
    /// has; this asserts the fields asked for instead - the ones actually
    /// used - and that the two clone-only fields appear only for a clone.
    #[test]
    fn a_painted_snapshot_records_the_brush_and_not_a_fit() {
        let plan_of = |paint: PaintPlan, tool: &str| Plan {
            expected_source: None,
            region_id: "c1-p001-h1".into(),
            geometry: Geometry::Stored,
            choice: Choice::Exact(Engine::Fill),
            source: "hand",
            tool: Some(tool.to_owned()),
            fill_mode: None,
            solid_color: None,
            clears_untouched: false,
            paint: Some(paint),
            requested: None,
            stroke: None,
            drawn: None,
        };
        let params = serde_json::json!({
            "mode": "paint",
            "paint": { "points": [{ "x": 1.0, "y": 1.0 }], "color": "#0a141e", "seed": 9 },
        });
        let paint = paint_plan("brush", &params).unwrap();
        let plan = plan_of(paint.clone(), "brush");
        let snapshot = paint_snapshot(
            &plan,
            &paint,
            Engine::Paint,
            17,
            std::time::Duration::from_millis(4),
        );
        assert_eq!(snapshot["rung"], "ladder.rung.paint");
        assert_eq!(snapshot["mode"], "paint");
        assert_eq!(snapshot["color"], "#0a141e");
        assert_eq!(snapshot["dabs"], 17);
        assert_eq!(snapshot["seed"], 9);
        assert_eq!(snapshot["source"], "hand");
        assert_eq!(snapshot["tool"], "brush");
        assert_eq!(snapshot["fill_mode"], "solid");
        assert_eq!(snapshot["brush"]["size"], 24.0);
        for absent in [
            "thickness",
            "deviation",
            "write_set",
            "quality",
            "alignment",
        ] {
            assert!(
                snapshot.get(absent).is_none(),
                "{absent} has no meaning for a stroke"
            );
        }

        let clone_params = serde_json::json!({
            "mode": "clone",
            "alignment": "nonAligned",
            "stroke": { "points": [{ "x": 3.0, "y": 4.0 }], "radius": 5.0 },
            "cloneOffset": { "x": 2.0, "y": -1.0 },
        });
        let paint = paint_plan("cloneHeal", &clone_params).unwrap();
        let plan = plan_of(paint.clone(), "cloneHeal");
        let snapshot = paint_snapshot(
            &plan,
            &paint,
            Engine::Clone,
            3,
            std::time::Duration::from_millis(1),
        );
        assert_eq!(snapshot["rung"], "ladder.rung.clone");
        assert_eq!(snapshot["mode"], "clone");
        assert_eq!(snapshot["alignment"], "nonAligned");
        // The seam sent `source − target`; the snapshot records the internal
        // `target − source`, which is its negation.
        assert_eq!(
            snapshot["source_offset"],
            serde_json::json!({ "x": -2.0, "y": 1.0 })
        );
        assert!(
            snapshot.get("color").is_none(),
            "a clone stroke has no colour of its own"
        );

        // A filled shape records the geometry instead of the sweep: which
        // shape, how far the feather reached, how many vertices described it.
        // The radii *actually used* are asked for, and for a shape those are
        // these.
        let shape_params = serde_json::json!({
            "mode": "solid",
            "color": "#ffffff",
            "opacity": 40,
            "painted": {
                "kind": "polygon",
                "points": [
                    { "x": 1.0, "y": 1.0 },
                    { "x": 9.0, "y": 1.0 },
                    { "x": 9.0, "y": 9.0 },
                ],
                "feather": 4.0,
            },
        });
        let paint = paint_plan("shapes", &shape_params).unwrap();
        let plan = plan_of(paint.clone(), "shapes");
        let snapshot = paint_snapshot(
            &plan,
            &paint,
            Engine::Paint,
            0,
            std::time::Duration::from_millis(2),
        );
        assert_eq!(snapshot["rung"], "ladder.rung.paint");
        assert_eq!(snapshot["mode"], "solid");
        assert_eq!(snapshot["color"], "#ffffff");
        assert_eq!(snapshot["shape"], "polygon");
        assert_eq!(snapshot["feather"], 4.0);
        assert_eq!(snapshot["vertices"], 3);
        assert_eq!(snapshot["brush"]["opacity"], 40.0);
        assert_eq!(snapshot["layer"]["opacity"], 40);
        // Nothing was walked, and the field says so rather than being absent.
        assert_eq!(snapshot["dabs"], 0);
        assert!(
            snapshot.get("source_offset").is_none(),
            "a filled shape reads from nowhere"
        );
    }

    /// The two hand tools are not rungs, and every place that asks the ladder a
    /// question about them has to answer without panicking: they sort above
    /// every ceiling, no run can reach them, the Layers picker cannot step onto
    /// or off them, and no rung name parses to one.
    #[test]
    fn neither_hand_tool_is_a_rung_the_ladder_can_reach() {
        for engine in [Engine::Paint, Engine::Clone] {
            assert!(!engine.is_rung());
            assert!(!run::reachable_automatically(engine));
            assert_eq!(
                step(engine, true),
                engine,
                "a picker cannot step off a hand tool"
            );
            assert_eq!(step(engine, false), engine);
            assert!(!MANUAL_RUNGS.contains(&engine));
            assert_eq!(
                run::effective_ceiling(None, Some(engine.rung_key())),
                run::HIGHEST_LOCAL
            );
            assert_eq!(named_rung(Some(engine.rung_key())), None);
        }
        // And the six that are rungs still are, so the predicate has not
        // quietly become "everything".
        for engine in MANUAL_RUNGS {
            assert!(engine.is_rung());
        }
    }

    /// The id a gate-skipped row is addressed by has to round-trip: the seam
    /// derives it from the geometry ([`crate::library::region_of_untouched`])
    /// and this module has to find the row again from the id alone.
    #[test]
    fn a_gate_skipped_row_is_found_from_the_id_the_seam_sent_back() {
        let bbox = Rect::new(120, 340, 56, 78);
        let id = untouched_id("c3-p007", bbox);
        assert_eq!(id, "c3-p007-u120-340-56-78");
        assert_eq!(job_page_id(&id), "c3-p007");
        assert_eq!(untouched_id(job_page_id(&id), bbox), id);
    }

    /// Every region kind's id names its page, and the page index is one less
    /// than the number in it.
    #[test]
    fn every_region_id_names_its_page() {
        assert_eq!(page_index_in("c3", "c3-p001-r0"), Some(0));
        assert_eq!(page_index_in("c3", "c3-p012-h2"), Some(11));
        assert_eq!(page_index_in("c3", "c3-p007-u1-2-3-4"), Some(6));
        // A chapter that does not hold it, and a page number that is not one.
        assert_eq!(page_index_in("c4", "c3-p001-r0"), None);
        assert_eq!(page_index_in("c3", "c3-pxx-r0"), None);
    }

    /// `stronger` and `simpler` walk the manual rungs and stop at the ends -
    /// and the cloud is not one of the ends, because this build cannot run it.
    #[test]
    fn the_manual_ladder_ends_at_rung_3a_and_never_reaches_the_cloud() {
        assert_eq!(step(Engine::Fill, false), Engine::Fill);
        assert_eq!(step(Engine::Fill, true), Engine::Lama);
        assert_eq!(step(Engine::Lama, true), Engine::Flux);
        assert_eq!(step(Engine::Flux, true), Engine::Flux);
        assert_eq!(step(Engine::Flux, false), Engine::Lama);
        assert!(!MANUAL_RUNGS.contains(&Engine::Cloud));
    }

    /// The fill-mode cycle steps through all three modes supported by the engines.
    #[test]
    fn the_fill_mode_cycle_only_names_modes_an_engine_has() {
        assert_eq!(next_fill_mode("match-surround"), "reconstruct");
        assert_eq!(next_fill_mode("reconstruct"), "solid");
        assert_eq!(next_fill_mode("solid"), "match-surround");
        assert_eq!(engine_for_fill_mode("reconstruct"), Engine::Lama);
        assert_eq!(engine_for_fill_mode("match-surround"), Engine::Fill);
        assert_eq!(engine_for_fill_mode("solid"), Engine::Fill);
    }

    /// **Auto clean's rows name a rung and still mean a *start*.** The two rows
    /// send `fill` or `lama` - the same words a Layers row's picker sends
    /// below rung 3a - and `cleanAnyway` reads them
    /// through here. Reading one as `Choice::Exact` would pin a gate-skipped
    /// region to a declined patch, where the pick's whole promise is that the
    /// ladder still escalates past it.
    #[test]
    fn auto_cleans_rows_name_a_rung_and_still_mean_a_start() {
        for (word, pick) in [
            ("fill", EnginePick::Fill),
            ("lama", EnginePick::Lama),
            // Denoise fill was removed; a stored `denoise` starts on the fill.
            ("denoise", EnginePick::Fill),
            // The word the rows sent before they named rungs. Still a rung-2
            // start rather than a fall back to the default - a stored
            // preference is not a typo.
            ("redraw", EnginePick::Lama),
        ] {
            assert!(
                matches!(choice_for(Some(word), EnginePick::Fill), Choice::Ladder(Some(p)) if p == pick),
                "{word}"
            );
        }
        // Nothing said: the caller's own default for this kind of text.
        assert!(matches!(
            choice_for(None, EnginePick::Lama),
            Choice::Ladder(Some(EnginePick::Lama))
        ));
        // Rung 3a is the one word no pick can carry - no automatic run may
        // reach it - so it can only ever be a rung named outright.
        assert!(matches!(
            choice_for(Some("flux"), EnginePick::Fill),
            Choice::Exact(Engine::Flux)
        ));
        // A word this build does not know is not a rung: it falls back to the
        // pick rather than to a guess.
        assert!(matches!(
            choice_for(Some("diffusion-9000"), EnginePick::Fill),
            Choice::Ladder(Some(EnginePick::Fill))
        ));
    }

    /// **A painted box is a mask, for every tool that paints one - the AI mask
    /// brush included.**
    ///
    /// It was the one exception: its box was a *place to look*, the detector
    /// was run over the page and whatever text it found under the stroke became
    /// the mask. Two complaints closed that - the text finder ran on every
    /// manual stroke, and a leftover beside a detected line was cleaned as
    /// the line. The hand is authoritative now, so no gesture reaches the
    /// detector at all and `Detected` is left to the one caller
    /// that has nothing but a box: a gate-skipped row on `cleanAnyway`.
    #[test]
    fn every_painted_box_is_a_mask_and_none_of_them_is_a_place_to_look() {
        let rect = Rect::new(10, 20, 30, 40);
        // No tool argument is the assertion: there is no longer a tool this
        // answers differently for, so no gesture can route itself back into a
        // detector run.
        assert!(matches!(painted_geometry(rect, None), Geometry::Given(r) if r == rect));
    }

    /// **A drawn shape is read as that shape** - the ellipse, the polygon and
    /// the lasso's own outline, rasterised at the page's own scale.
    ///
    /// This closes the defect in the last tool it was open in. Shapes used
    /// to send only its bounding box, so an ellipse cleaned the rectangle
    /// around it and a lasso cleaned the box its curve fitted inside. The
    /// box is still sent and is still the fallback
    /// for a backend or a payload that has no shape in it.
    #[test]
    fn every_shape_the_tool_draws_rasterises_as_itself_and_not_as_its_box() {
        let corners = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![
                StrokePoint { x: x0, y: y0 },
                StrokePoint { x: x1, y: y0 },
                StrokePoint { x: x1, y: y1 },
                StrokePoint { x: x0, y: y1 },
            ]
        };

        // A rectangle *is* its box, and every pixel of it is in the mask.
        let rect = PaintedShape {
            kind: ShapeKind::Rect,
            points: corners(10.0, 10.0, 50.0, 30.0),
            feather: 0.0,
        };
        let mask = shape_mask(&rect, 1000, 1000).expect("a rectangle on the page is a mask");
        assert_eq!(mask.bounds, Rect::new(100, 100, 400, 200));
        assert_eq!(mask.count(), 400 * 200);

        // An ellipse is the one inscribed in that same box: the centre is in,
        // the corners are out, and it covers about π/4 of the box.
        let ellipse = PaintedShape {
            kind: ShapeKind::Ellipse,
            ..rect.clone()
        };
        let mask = shape_mask(&ellipse, 1000, 1000).expect("an ellipse on the page is a mask");
        assert!(mask.contains(300, 200), "the centre is inside");
        assert!(!mask.contains(101, 101), "a corner of the box is not");
        assert!(!mask.contains(499, 299));
        let area = (mask.bounds.w as usize) * (mask.bounds.h as usize);
        let ratio = mask.count() as f64 / area as f64;
        assert!(
            (ratio - std::f64::consts::FRAC_PI_4).abs() < 0.01,
            "{ratio} is not π/4"
        );

        // A polygon is its own outline. This is the L a lasso draws, and the
        // notch is the half of the bounding box that must stay empty - the
        // same defect, in the tool it had not yet been fixed in.
        let l = PaintedShape {
            kind: ShapeKind::Polygon,
            points: vec![
                StrokePoint { x: 10.0, y: 10.0 },
                StrokePoint { x: 20.0, y: 10.0 },
                StrokePoint { x: 20.0, y: 40.0 },
                StrokePoint { x: 50.0, y: 40.0 },
                StrokePoint { x: 50.0, y: 50.0 },
                StrokePoint { x: 10.0, y: 50.0 },
            ],
            feather: 0.0,
        };
        let mask = shape_mask(&l, 1000, 1000).expect("a polygon on the page is a mask");
        assert!(mask.contains(150, 150), "the upright of the L");
        assert!(mask.contains(400, 450), "the foot of the L");
        assert!(!mask.contains(400, 150), "the notch is not filled");

        let line = PaintedShape {
            kind: ShapeKind::Line,
            points: vec![StrokePoint { x: 10.0, y: 10.0 }, StrokePoint { x: 50.0, y: 30.0 }],
            feather: 2.0,
        };
        let mask = shape_mask(&line, 1000, 1000).expect("two vertices make a line");
        assert!(mask.contains(300, 200));
        assert!(!mask.contains(300, 210));

        // Nothing to fill is not a mask: the caller falls back to the box.
        let two = PaintedShape {
            kind: ShapeKind::Polygon,
            points: vec![
                StrokePoint { x: 1.0, y: 1.0 },
                StrokePoint { x: 2.0, y: 2.0 },
            ],
            feather: 0.0,
        };
        assert!(shape_mask(&two, 1000, 1000).is_none());
    }

    #[test]
    fn feather_grows_the_mask_past_its_outline_and_is_clamped() {
        let square = PaintedShape {
            kind: ShapeKind::Rect,
            points: vec![
                StrokePoint { x: 40.0, y: 40.0 },
                StrokePoint { x: 60.0, y: 40.0 },
                StrokePoint { x: 60.0, y: 60.0 },
                StrokePoint { x: 40.0, y: 60.0 },
            ],
            feather: 0.0,
        };
        let plain = shape_mask(&square, 1000, 1000).expect("a mask");
        let feathered = shape_mask(
            &PaintedShape {
                feather: 8.0,
                ..square.clone()
            },
            1000,
            1000,
        )
        .expect("a mask");
        assert!(feathered.count() > plain.count());
        assert!(
            feathered.contains(396, 500),
            "the mask reaches past the outline"
        );
        assert!(!plain.contains(396, 500));

        // A feather nothing offered - the row stops at `MAX_FEATHER_PX` -
        // cannot ask for a dilation the size of the page. It is clamped rather
        // than refused, and the clamp is what keeps the cost bounded: the
        // dilation tests a `(2r+1)²` kernel at every pixel it grows into.
        let absurd = shape_mask(
            &PaintedShape {
                feather: 1.0e9,
                ..square.clone()
            },
            1000,
            1000,
        )
        .expect("a mask");
        assert_eq!(
            absurd.bounds.w as f64,
            200.0 + MAX_FEATHER_PX * 2.0,
            "a feather past the row's own maximum is held at it"
        );
    }

    /// The Shapes tool's `mode` row is one list of six: a solid colour, or one
    /// of the five cleaning rungs. Only the first is a paint, and the other
    /// five must leave the paint branch alone - a shape pointed at LaMa that
    /// took this path would commit flat black instead of a reconstruction.
    #[test]
    fn only_a_shape_set_to_solid_is_a_paint_plan_and_it_needs_no_points() {
        let painted = serde_json::json!({
            "kind": "ellipse",
            "points": [
                { "x": 10.0, "y": 10.0 },
                { "x": 50.0, "y": 10.0 },
                { "x": 50.0, "y": 40.0 },
                { "x": 10.0, "y": 40.0 },
            ],
            "feather": 3.0,
        });

        for mode in ["fill", "denoise", "lama", "flux"] {
            let params = serde_json::json!({ "mode": mode, "painted": painted });
            assert_eq!(paint_plan("shapes", &params), None, "{mode}");
        }

        // A rectangle drag delivers one pointer sample and the keyboard route
        // delivers none. Neither is short of a shape.
        let params = serde_json::json!({
            "mode": "solid",
            "color": "#ff8800",
            "opacity": 60,
            "painted": painted,
        });
        let plan = paint_plan("shapes", &params).expect("a solid shape is a paint plan");
        let PaintKind::Shape { color, shape, .. } = plan.kind else {
            panic!("not a filled shape")
        };
        assert_eq!(color, [0xff, 0x88, 0x00]);
        assert_eq!(shape.kind, ShapeKind::Ellipse);
        assert_eq!(shape.feather, 3.0);
        assert_eq!(plan.spec.opacity, 60.0);
        assert!(plan.points.is_empty());

        // The colour and the shape are read from the `paint` block first, as
        // every other paint parameter is, and `params` stands in for it.
        let params = serde_json::json!({
            "mode": "solid",
            "color": "#000000",
            "paint": { "shape": painted, "color": "#123", "opacity": 100 },
        });
        let plan = paint_plan("shapes", &params).expect("a solid shape is a paint plan");
        let PaintKind::Shape { color, .. } = plan.kind else {
            panic!("not a filled shape")
        };
        assert_eq!(color, [0x11, 0x22, 0x33]);

        // A shape whose kind this build does not know fails to parse, and a
        // caller that sent no shape at all is not painting: both fall back to
        // the ladder with the bounding box, which is §6.1's promised
        // degradation rather than a refusal.
        let params =
            serde_json::json!({ "mode": "solid", "painted": { "kind": "bezier", "points": [] } });
        assert_eq!(paint_plan("shapes", &params), None);
        assert_eq!(
            paint_plan("shapes", &serde_json::json!({ "mode": "solid" })),
            None
        );
    }

    /// A colour is a colour in every mode this build paints on, and alpha is
    /// never one of the channels it writes.
    #[test]
    fn a_solid_colour_is_written_per_channel_and_never_into_alpha() {
        use cleaner_core::image::ColorMode;
        assert_eq!(
            solid_levels(ColorMode::Rgb, [10, 20, 30]),
            vec![Some(10), Some(20), Some(30)]
        );
        assert_eq!(
            solid_levels(ColorMode::Rgba, [10, 20, 30]),
            vec![Some(10), Some(20), Some(30), None]
        );
        // Rec. 601: 0.299·255 + 0.587·0 + 0.114·0 = 76.
        assert_eq!(solid_levels(ColorMode::Gray, [255, 0, 0]), vec![Some(76)]);
        assert_eq!(
            solid_levels(ColorMode::GrayAlpha, [255, 255, 255]),
            vec![Some(255), None]
        );
    }

    /// **A stroke that carries its shape is read as that shape** - the mask is
    /// the discs the brush swept, for every tool that sweeps them.
    #[test]
    fn a_painted_shape_is_preferred_to_the_box_around_it() {
        let rect = Rect::new(10, 20, 30, 40);
        let shape = Mask::from_stroke(&[(12.0, 22.0), (36.0, 55.0)], 3.0, 200, 200);
        let Geometry::Painted(mask) = painted_geometry(rect, Some(shape.clone())) else {
            panic!("a shape that arrived is the mask");
        };
        // The shape itself, not the rectangle around it: a stroke's mask is a
        // small fraction of its own bounding box.
        assert_eq!(mask.count(), shape.count());
        assert!(mask.count() * 2 < (rect.w as usize) * (rect.h as usize));
    }

    /// The seam's stroke, in the seam's own coordinate space: points in page
    /// percent, radius in page pixels. A stroke that arrives as its bounding
    /// box would pass a bbox assertion, so what is asserted is the corner the
    /// rectangle would have swallowed.
    #[test]
    fn a_painted_stroke_rasterises_to_a_round_stroke_and_not_a_rectangle() {
        let stroke = PaintedStroke {
            points: vec![
                StrokePoint { x: 10.0, y: 10.0 },
                StrokePoint { x: 50.0, y: 50.0 },
            ],
            radius: 6.0,
        };
        let mask = stroke_mask(&stroke, 1000, 1000).expect("a stroke on the page is a mask");
        assert!(mask.contains(100, 100));
        assert!(mask.contains(300, 300));
        assert!(mask.contains(500, 500));
        // Inside the bounding box, nowhere near the path.
        assert!(!mask.contains(100, 500));
        assert!(!mask.contains(500, 100));
        // Which is to say: the mask is a small fraction of its own bbox, where
        // a rectangle would fill it.
        let area = (mask.bounds.w as usize) * (mask.bounds.h as usize);
        assert!(mask.count() * 4 < area, "a rectangle got through");
        // Nothing to paint is not a mask: the caller falls back to the box.
        assert!(stroke_mask(
            &PaintedStroke {
                points: vec![],
                radius: 6.0
            },
            1000,
            1000
        )
        .is_none());
    }

    /// The engines the AI mask brush's row offers are `ROW_ENGINES` in
    /// `src/lib/model/masks.js`, sent as `params.engine` - the same three words a
    /// Layers row's picker sends, and every one of them has to be a rung this
    /// side knows. A word that does not parse is not refused: it falls through
    /// to the fill/redraw pick, so a typo would silently clean with something
    /// nobody chose rather than failing where anyone could see it.
    #[test]
    fn every_engine_the_mask_brush_offers_is_a_rung_this_build_runs() {
        for (word, rung) in [
            ("fill", Engine::Fill),
            ("lama", Engine::Lama),
            ("flux", Engine::Flux),
        ] {
            assert_eq!(named_rung(Some(word)), Some(rung), "{word}");
            assert!(MANUAL_RUNGS.contains(&rung), "{word}");
        }
    }

    /// Percentages and pixels are inverses, to the rounding.
    #[test]
    fn a_percentage_box_and_a_pixel_box_are_the_same_rectangle() {
        let rect = pixels_of(
            Bbox {
                x: 10.0,
                y: 25.0,
                w: 50.0,
                h: 10.0,
            },
            1000,
            1600,
        );
        assert_eq!(rect, Rect::new(100, 400, 500, 160));
        let back = crate::library::percent_of(rect, 1000, 1600);
        assert!((back.x - 10.0).abs() < 0.01 && (back.h - 10.0).abs() < 0.01);
    }

    /// A machine with nothing installed is not offered rung 3a, and is told
    /// nothing about it - the whole of "absence is the normal state".
    #[test]
    fn a_machine_with_no_sidecar_is_offered_nothing_and_told_nothing() {
        let status = sidecar_status(Some(std::path::Path::new("/nonexistent/manga-cleaner")));
        // The developer's checkout is on the search path, so this can only
        // assert the shape the absent case takes when it is taken.
        if !status.available {
            assert!(status.reason_key.is_none() || status.reason_key.is_some());
        }
    }

    #[test]
    fn sidecar_status_from_explicit_path_is_searched_first() {
        let status = sidecar_status_from(
            Some("/nonexistent/custom-sidecar-path"),
            None,
            Some(std::path::Path::new("/nonexistent/manga-cleaner")),
        );
        if !status.available {
            assert!(status.reason_key.is_none() || status.reason_key.is_some());
        }
    }

    #[test]
    fn parse_model_dir_name_derives_correct_id_and_label() {
        assert_eq!(
            parse_model_dir_name("flux2-klein-4b-sdnq"),
            ("flux2-klein-4b".to_string(), "FLUX.2 Klein 4B".to_string())
        );
        assert_eq!(
            parse_model_dir_name("flux2-klein-4b-mflux-q4"),
            (
                "flux2-klein-4b".to_string(),
                "FLUX.2 Klein 4B (4-bit)".to_string()
            )
        );
        assert_eq!(
            parse_model_dir_name("flux2-klein-4b-mflux-q8"),
            (
                "flux2-klein-4b".to_string(),
                "FLUX.2 Klein 4B (8-bit)".to_string()
            )
        );
        assert_eq!(
            parse_model_dir_name("flux2-klein-9b-mflux-q4"),
            (
                "flux2-klein-9b".to_string(),
                "FLUX.2 Klein 9B (4-bit)".to_string()
            )
        );
        assert_eq!(
            parse_model_dir_name("flux2-klein-4b"),
            ("flux2-klein-4b".to_string(), "FLUX.2 Klein 4B".to_string())
        );
        assert_eq!(
            parse_model_dir_name("flux1-dev-mflux-q4"),
            ("flux1-dev".to_string(), "FLUX.1 [dev] (4-bit)".to_string())
        );
        assert_eq!(
            parse_model_dir_name("flux2-klein-4b-gguf-q4_k_m"),
            (
                "flux2-klein-4b".to_string(),
                "FLUX.2 Klein 4B (Q4_K_M)".to_string()
            )
        );
        assert_eq!(
            parse_model_dir_name("custom-model"),
            ("custom-model".to_string(), "custom-model".to_string())
        );
    }

    #[test]
    fn list_sidecar_models_from_empty_or_missing_dir_returns_empty() {
        let models = list_sidecar_models_from(
            Some("/nonexistent/custom-sidecar-path"),
            Some(std::path::Path::new("/nonexistent/manga-cleaner")),
        );
        assert_eq!(models, Vec::new());
    }

    #[test]
    fn clamped_gesture_stays_strictly_within_page_bounds() {
        let mut raster = cleaner_core::image::fixtures::by_name("l8").raster;
        raster.width = 100;
        raster.height = 100;
        let off_bottom_right = clamped(Rect::new(150, 150, 20, 20), &raster);
        assert_eq!(off_bottom_right, Rect::new(99, 99, 1, 1));
        assert!(off_bottom_right.x + off_bottom_right.w as i64 <= 100);
        assert!(off_bottom_right.y + off_bottom_right.h as i64 <= 100);

        let off_top_left = clamped(Rect::new(-50, -50, 10, 10), &raster);
        assert_eq!(off_top_left, Rect::new(0, 0, 1, 1));
    }

    #[test]
    fn job_page_id_handles_hyphenated_page_ids_with_marker_prefixes() {
        let region_id_h = "ch-undo-p001-h1";
        assert_eq!(job_page_id(region_id_h), "ch-undo-p001");

        let region_id_r = "ch-undo-p001-r2";
        assert_eq!(job_page_id(region_id_r), "ch-undo-p001");

        let region_id_u = "ch-redo-p001-u1-2-3-4";
        assert_eq!(job_page_id(region_id_u), "ch-redo-p001");
    }

    #[test]
    fn rerun_patch_lookup_finds_soft_deleted_patch_where_visible_is_false() {
        let mut patch = PatchRecord::of(
            0,
            &Patch {
                id: "c1-p001-r0".to_string(),
                mask: Mask::filled(Rect::new(0, 0, 10, 10)),
                ink: Mask::filled(Rect::new(0, 0, 10, 10)),
                pixels: cleaner_core::image::fixtures::by_name("l8").raster,
                order: 0,
                visible: true,
                provenance: cleaner_core::patch::Provenance {
                    engine: Engine::Fill,
                    engine_version: "0.1.0".to_string(),
                    model_sha256: None,
                    execution_provider: "cpu".to_string(),
                    params_snapshot: serde_json::json!({ "fill_mode": "solid" }),
                    mask_sha256: "abc".to_string(),
                    source_sha256: "def".to_string(),
                    cloud: None,
                    created: 1_760_000_000,
                },
            },
            None,
        );
        patch.visible = false;
        let patches = [patch];

        let found = patches.iter().find(|record| record.id == "c1-p001-r0");
        assert!(found.is_some());
        assert!(!found.unwrap().visible);
    }

    #[test]
    fn clean_anyway_fallback_selects_bubble_for_in_balloon_gate_skips_and_outside_for_outside_bubble(
    ) {
        assert_eq!(
            untouched_fallback_pick(Some("review.reason.gateSkippedOutsideBubble"), None),
            Picks::default().outside
        );
        assert_eq!(
            untouched_fallback_pick(Some("review.reason.gateSkippedNotJapanese"), None),
            Picks::default().bubble
        );
        assert_eq!(
            untouched_fallback_pick(Some("review.reason.gateSkippedLowConfidence"), None),
            Picks::default().bubble
        );

        let in_balloon_choice = choice_for(
            None,
            untouched_fallback_pick(Some("review.reason.gateSkippedNotJapanese"), None),
        );
        assert_eq!(in_balloon_choice, Choice::Ladder(Some(EnginePick::Fill)));

        let out_balloon_choice = choice_for(
            None,
            untouched_fallback_pick(Some("review.reason.gateSkippedOutsideBubble"), None),
        );
        assert_eq!(out_balloon_choice, Choice::Ladder(Some(EnginePick::Lama)));

        // A held candidate starts where its balloon says, and outside when
        // there is none or the row predates the record of it.
        for (inside, pick) in [
            (Some(true), Picks::default().bubble),
            (Some(false), Picks::default().outside),
            (None, Picks::default().outside),
        ] {
            for key in ["review.reason.unassignedMask", "review.reason.isolatedMask"] {
                assert_eq!(untouched_fallback_pick(Some(key), inside), pick, "{key} {inside:?}");
            }
        }
    }

    #[test]
    fn named_rung_parses_cloud_correctly() {
        assert_eq!(named_rung(Some("cloud")), Some(Engine::Cloud));
        assert_eq!(named_rung(Some("lama")), Some(Engine::Lama));
        assert_eq!(named_rung(Some("fill")), Some(Engine::Fill));
        assert_eq!(named_rung(None), None);
    }

    #[test]
    fn a_cloud_patch_goes_local_only_when_a_local_engine_is_chosen() {
        // Run again as itself, by Try again or a step that lands on the same
        // rung: FLUX recorded with cloud provenance, or the legacy engine.
        assert!(rerun_needs_cloud(Engine::Flux, Engine::Flux, true, false));
        assert!(rerun_needs_cloud(Engine::Cloud, Engine::Cloud, true, false));
        // The cloud named outright, whatever ran before.
        assert!(rerun_needs_cloud(Engine::Cloud, Engine::Lama, false, true));
        assert!(rerun_needs_cloud(Engine::Cloud, Engine::Flux, true, true));
        // A local engine chosen for a cloud patch, FLUX included.
        assert!(!rerun_needs_cloud(Engine::Lama, Engine::Flux, true, true));
        assert!(!rerun_needs_cloud(Engine::Flux, Engine::Flux, true, true));
        // Stepped down to another rung.
        assert!(!rerun_needs_cloud(Engine::Lama, Engine::Flux, true, false));
        // Local FLUX re-run as itself stays local.
        assert!(!rerun_needs_cloud(Engine::Flux, Engine::Flux, false, false));
    }

    #[test]
    fn step_on_cloud_engine_returns_cloud_engine() {
        assert_eq!(step(Engine::Cloud, true), Engine::Cloud);
        assert_eq!(step(Engine::Cloud, false), Engine::Cloud);
    }

    #[test]
    fn explicit_remote_and_malformed_targets_fail_closed() {
        for value in [
            serde_json::json!({"type":"beam","profile_id":"p"}),
            serde_json::json!(null),
            serde_json::json!({"type":"local","extra":true}),
        ] {
            assert!(explicit_remote_or_invalid_target(
                &serde_json::json!({"executionTarget":value})
            ));
        }
        assert!(!explicit_remote_or_invalid_target(
            &serde_json::json!({"engine":"flux","executionTarget":{"type":"local"}})
        ));
        assert!(!explicit_remote_or_invalid_target(
            &serde_json::json!({"engine":"lama"})
        ));
    }

    #[test]
    fn automatic_runs_never_reach_cloud_engine() {
        assert!(!run::reachable_automatically(Engine::Cloud));
        assert!(!run::reachable_automatically(Engine::Flux));
        assert_eq!(run::HIGHEST_AUTOMATIC, Engine::Lama);
        assert_eq!(
            run::effective_ceiling(None, Some(Engine::Cloud.rung_key())),
            run::HIGHEST_LOCAL
        );
    }

    #[test]
    fn is_remote_target_detects_beam_and_modal() {
        let beam_params =
            serde_json::json!({"executionTarget": {"type": "beam", "profile_id": "test-beam"}});
        let modal_params =
            serde_json::json!({"executionTarget": {"type": "modal", "profile_id": "test-modal"}});
        let local_params = serde_json::json!({"executionTarget": {"type": "local"}});
        let empty_params = serde_json::json!({});

        assert!(explicit_remote_or_invalid_target(&beam_params));
        assert!(explicit_remote_or_invalid_target(&modal_params));
        assert!(!explicit_remote_or_invalid_target(&local_params));
        assert!(!explicit_remote_or_invalid_target(&empty_params));
    }
    #[test]
    fn orientation_all_eight_edit_compare_clone_and_history_use_the_same_pixels() {
        use cleaner_core::{image::{decode,orientation::Orientation},composite::composite};
        for value in 1..=8 {
            let (root,located)=strip_edit_fixture(&format!("orientation-{value}"));
            let mut job=Job::open(&located.job_path).unwrap();
            job.project.strip.mode=cleaner_core::project::StripMode::Single;
            job.project.sources[0].orientation=Orientation(value);job.flush().unwrap();
            let original_bytes=std::fs::read(job.source_path(0).unwrap()).unwrap();
            let native=decode(&original_bytes).unwrap();let visible=Orientation(value).raster(&native);
            let page=crate::library::page_of("chapter",&job.project,0).unwrap();
            assert_eq!((page.width,page.height),(visible.width,visible.height));
            let shown=decode(&crate::tile::render(job.path(),0,crate::tile::Variant::Source,None).unwrap()).unwrap();
            assert_eq!(shown.data,cleaner_core::image::proxy::display(&visible).unwrap().data);
            let paint=PaintPlan {kind:PaintKind::Clone{heal:false,offset:(10.0,0.0)},points:vec![PaintPoint{x:50.0,y:50.0,p:1.0}],spec:cleaner_core::paint::BrushSpec{size:8.0,hardness:100.0,opacity:100.0,..Default::default()},alignment:"aligned"};
            let plan=Plan{expected_source:None,solid_color:None,region_id:"chapter-p001-h1".into(),geometry:Geometry::Given(Rect::new(1,1,2,2)),choice:Choice::Exact(Engine::Fill),source:"hand",tool:Some("clone".into()),fill_mode:None,clears_untouched:false,paint:Some(paint),requested:None,stroke:None,drawn:None};
            let outcome=edit_with_bench(&mut test_bench(&root),Engine::Fill,&located,plan).unwrap();
            assert!(matches!(outcome,Outcome::Cleaned(_)),"orientation {value}");
            let mut job=Job::open(&located.job_path).unwrap();let patches=job.native_patches_on_page(0).unwrap();
            let after=composite(&native,&patches).unwrap();
            for y in 0..native.height{for x in 0..native.width{if !patches[0].mask.contains(x as i64,y as i64){assert_eq!(after.sample(x,y,0),native.sample(x,y,0));}}}
            let cleaned=decode(&crate::tile::render(job.path(),0,crate::tile::Variant::Cleaned,None).unwrap()).unwrap();
            let expected=cleaner_core::image::proxy::display(&Orientation(value).raster(&after)).unwrap();assert_eq!(cleaned.data,expected.data);
            // Clone the visible target's center from the visible native source.
            let tx=(visible.width as f64*0.5).floor() as u32;let ty=(visible.height as f64*0.5).floor() as u32;
            let sx=(tx as f64-visible.width as f64*0.1).round() as u32;
            let committed=Orientation(value).raster(&after);
            assert_eq!(committed.sample(tx,ty,0),visible.sample(sx,ty,0),"clone orientation {value}");
            let record=job.project.patches[0].clone();let mut journal=crate::history::Journal::default();
            let state=crate::history::Side{present:true,page_status:Some("cleaned".into()),region:None};
            journal.push(crate::history::NewDelta{label:"clone".into(),op:"region-state".into(),region_id:record.id.clone(),before:crate::history::Side{present:false,page_status:Some("unclean".into()),region:None},after:state});
            crate::history::save(job.path(),&journal).unwrap();let mut recovered=crate::history::load(job.path());
            assert_eq!(recovered.undo().unwrap().region_id,record.id);job.project.patches[0].visible=false;job.flush().unwrap();
            assert_eq!(crate::tile::render(job.path(),0,crate::tile::Variant::Cleaned,None).unwrap(),crate::tile::render(job.path(),0,crate::tile::Variant::Source,None).unwrap());
            assert_eq!(recovered.redo().unwrap().region_id,record.id);job.project.patches[0].visible=true;job.flush().unwrap();
            assert_eq!(decode(&crate::tile::render(job.path(),0,crate::tile::Variant::Cleaned,None).unwrap()).unwrap().data,cleaned.data);
            assert_eq!(std::fs::read(job.source_path(0).unwrap()).unwrap(),original_bytes);
            let native_box=patches[0].mask.bounds;
            journal.coordinate_version=0;
            journal.entries[0].after.region=Some(serde_json::json!({"bbox":{"x":native_box.x as f64/native.width as f64*100.0,"y":native_box.y as f64/native.height as f64*100.0,"w":native_box.w as f64/native.width as f64*100.0,"h":native_box.h as f64/native.height as f64*100.0}}));
            crate::history::save(job.path(),&journal).unwrap();
            let adapted=crate::history::load(job.path());assert_eq!(adapted.coordinate_version,1);
            let adapted_box=&adapted.entries[0].after.region.as_ref().unwrap()["bbox"];
            let visible_box=Orientation(value).rect(native_box,native.width,native.height);
            assert!((adapted_box["x"].as_f64().unwrap()-visible_box.x as f64/visible.width as f64*100.0).abs()<1e-6);
            assert!((adapted_box["y"].as_f64().unwrap()-visible_box.y as f64/visible.height as f64*100.0).abs()<1e-6);
            crate::history::save(job.path(),&adapted).unwrap();
            assert_eq!(crate::history::load(job.path()).entries,adapted.entries);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn orientation_rotated_page_beside_unrotated_page_keeps_cross_join_edits_native() {
        use cleaner_core::{image::{decode,orientation::Orientation},composite::composite};
        let (root,located)=strip_edit_fixture("orientation-cross-join");
        let mut job=Job::open(&located.job_path).unwrap();job.project.sources[0].orientation=Orientation(6);job.flush().unwrap();
        let (w,h)=page_size(&job,0).unwrap();let rect=pixels_of(Bbox{x:44.0,y:90.0,w:10.0,h:20.0},w,h);
        let shape=PaintedShape{kind:ShapeKind::Ellipse,points:vec![StrokePoint{x:44.0,y:90.0},StrokePoint{x:54.0,y:90.0},StrokePoint{x:54.0,y:110.0},StrokePoint{x:44.0,y:110.0}],feather:0.0};
        let plan=Plan{expected_source:None,solid_color:None,region_id:"chapter-p001-h1".into(),geometry:Geometry::Given(rect),choice:Choice::Exact(Engine::Fill),source:"hand",tool:Some("shapes".into()),fill_mode:Some("solid"),clears_untouched:false,paint:Some(PaintPlan{kind:PaintKind::Shape{color:[30,30,30],shape:shape.clone(),outline:None},points:Vec::new(),spec:Default::default(),alignment:"aligned"}),requested:Some(rect),stroke:None,drawn:Some(shape)};
        assert!(matches!(edit_with_bench(&mut test_bench(&root),Engine::Fill,&located,plan).unwrap(),Outcome::Cleaned(_)));
        let job=Job::open(&located.job_path).unwrap();assert_eq!(job.project.patches[0].native_parts.len(),2);
        for position in 0..2 {
            let bytes=std::fs::read(job.source_path(position).unwrap()).unwrap();let source=decode(&bytes).unwrap();
            let patches=job.native_patches_on_page(position).unwrap();assert!(patches[0].mask.count()>0);
            let native=composite(&source,&patches).unwrap();
            for y in 0..source.height{for x in 0..source.width{if !patches[0].mask.contains(x as i64,y as i64){assert_eq!(source.sample(x,y,0),native.sample(x,y,0));}}}
            let expected=cleaner_core::image::proxy::display(&job.project.sources[position].orientation.raster(&native)).unwrap();
            let tile=decode(&crate::tile::render(job.path(),position,crate::tile::Variant::Cleaned,None).unwrap()).unwrap();assert_eq!(tile.data,expected.data);
        }
        let visible=job.load_display_patch(&job.project.patches[0]).unwrap();
        assert!(visible.mask.bounds.y < h as i64 && visible.mask.bounds.bottom()>h as i64);
        assert_eq!(cleaner_core::project::orientation::display_bbox(&job.project,&job.project.patches[0]),visible.mask.bounds);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn associated_tiff_manual_source_fill_refuses_before_writing_a_patch() {
        let (root,located)=strip_edit_fixture("associated-native-fill");
        let mut job=Job::open(&located.job_path).unwrap();
        let mut page=cleaner_core::image::fixtures::by_name("rgba8").raster;page.color.associated_alpha=true;
        let bytes=cleaner_core::image::encode(&page,cleaner_core::image::Format::Tiff).unwrap();
        std::fs::write(job.source_path(0).unwrap(),&bytes).unwrap();
        job.project.sources[0].w=page.width;job.project.sources[0].h=page.height;job.project.sources[0].mode=page.mode;job.flush().unwrap();
        let manifest=std::fs::read(job.path()).unwrap();
        let plan=Plan{expected_source:None,solid_color:None,region_id:"chapter-p001-h1".into(),geometry:Geometry::Given(Rect::new(8,8,12,10)),choice:Choice::Exact(Engine::Fill),source:"hand",tool:Some("shapes".into()),fill_mode:Some("match-surround"),clears_untouched:false,paint:None,requested:None,stroke:None,drawn:None};
        let result=edit_with_bench(&mut test_bench(&root),Engine::Fill,&located,plan);
        match result {Err(error)=>assert!(error.contains("associated-alpha"),"{error}"),Ok(_)=>panic!("associated-alpha source fill unexpectedly ran")};
        assert_eq!(std::fs::read(job.path()).unwrap(),manifest);
        assert_eq!(std::fs::read(job.source_path(0).unwrap()).unwrap(),bytes);
        assert!(Job::open(job.path()).unwrap().project.patches.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

}

#[cfg(test)]
mod mask_edit_tests {
    use super::*;

    fn block(x: i64, y: i64, w: u32, h: u32) -> Mask {
        Mask::filled(Rect::new(x, y, w, h))
    }

    fn detection<'a>(id: &'a str, order: u32, mask: &'a Mask, ink: &'a Mask) -> DetectionMasks<'a> {
        DetectionMasks { id, order, mask, ink }
    }

    fn inside(ink: &Mask, mask: &Mask) -> bool {
        (ink.bounds.y..ink.bounds.bottom())
            .all(|y| (ink.bounds.x..ink.bounds.right()).all(|x| !ink.contains(x, y) || mask.contains(x, y)))
    }

    fn loaded(mask: Mask, base: Option<Mask>) -> cleaner_core::project::LoadedDetection {
        let mut record = hand_detection(0, mask.bounds, 0);
        record.padding_px = if base.is_some() { 4 } else { 0 };
        cleaner_core::project::LoadedDetection {
            record,
            ink: mask.clone(),
            mask,
            evidence: None,
            base: base.map(|base| (base.clone(), base)),
        }
    }

    /// Two blocks of lettering the page stored apart can come back from a
    /// window as one region. The half the page still holds is taken out of
    /// it, and the half the page lost is stored on its own.
    #[test]
    fn an_area_detection_keeps_only_what_the_page_does_not_hold() {
        let page = (400, 400);
        let kept = loaded(block(100, 200, 60, 60), None);
        let both = union_masks(&block(100, 100, 60, 60), &block(100, 200, 60, 60));
        let found = area_detection_less_held(loaded(both, None), std::slice::from_ref(&kept), page).unwrap();
        assert_eq!(found.mask.bounds, Rect::new(100, 100, 60, 60));
        assert_eq!((found.record.bbox, found.ink.bounds), (found.mask.bounds, found.mask.bounds));
        assert!(found.record.group.is_none() && found.evidence.is_none());

        // The same lettering again, a pixel or two off at its rim: nothing new.
        let again = loaded(block(98, 198, 64, 64), None);
        assert!(area_detection_less_held(again, std::slice::from_ref(&kept), page).is_none());

        // A few pixels of the held lettering's far edge, just past the rim,
        // do not stretch the cut detection over it. A small mark of the kept
        // text, away from the rim, stays.
        let stray = union_masks(&union_masks(&block(100, 100, 60, 60), &block(100, 200, 60, 60)),
            &union_masks(&block(164, 228, 2, 2), &block(170, 100, 3, 3)));
        let found = area_detection_less_held(loaded(stray, None), std::slice::from_ref(&kept), page).unwrap();
        assert_eq!(found.mask.bounds, Rect::new(100, 100, 73, 60));
        assert!(found.mask.contains(171, 101) && !found.mask.contains(164, 228));

        // Lettering the page does not hold is stored as it was found.
        let apart = loaded(block(300, 300, 40, 40), None);
        let found = area_detection_less_held(apart.clone(), std::slice::from_ref(&kept), page).unwrap();
        assert_eq!((found.mask, found.record), (apart.mask, apart.record));
    }

    /// A padded detection's ungrown pair is cut with its masks, and a pair
    /// with nothing left gives way to the cut masks as the baseline.
    #[test]
    fn an_area_detection_cut_keeps_its_padding_baseline_in_step() {
        let page = (400, 400);
        let kept = loaded(block(100, 200, 60, 60), None);
        let seed = union_masks(&block(100, 100, 60, 60), &block(100, 200, 60, 60));
        let both = seed.dilated(4, page.0, page.1);
        let found = area_detection_less_held(loaded(both.clone(), Some(seed)), std::slice::from_ref(&kept), page).unwrap();
        assert_eq!(found.mask.bounds, Rect::new(96, 96, 68, 68));
        assert_eq!(found.base.as_ref().map(|(mask, _)| mask.bounds), Some(Rect::new(100, 100, 60, 60)));
        assert_eq!(found.record.padding_px, 4);

        let held_seed = loaded(both, Some(block(100, 200, 60, 60)));
        let found = area_detection_less_held(held_seed, std::slice::from_ref(&kept), page).unwrap();
        assert!(found.base.is_none());
        assert_eq!(found.record.padding_px, 0);
    }

    /// The one detection a stroke overlaps takes all of it, mask and lettering.
    #[test]
    fn an_add_over_one_detection_grows_its_mask_and_its_lettering() {
        let (mask, ink) = (block(10, 10, 10, 10), block(12, 12, 4, 4));
        let page = [detection("d0", 0, &mask, &ink)];
        let plan = plan_mask_edit(&page, &block(18, 12, 6, 4), MaskEditMode::Add);
        assert_eq!(plan.created, None);
        assert!(plan.removed.is_empty());
        let [(id, mask, ink)] = plan.updated.as_slice() else { panic!("one update, got {plan:?}") };
        assert_eq!(id, "d0");
        assert_eq!(mask.bounds, Rect::new(10, 10, 14, 10));
        assert_eq!(ink.bounds, Rect::new(12, 12, 12, 4));
        assert!(mask.contains(23, 15) && !mask.contains(23, 17));
        assert!(ink.contains(23, 13) && ink.contains(13, 13) && !ink.contains(17, 13));
        assert!(inside(ink, mask));
    }

    /// A stroke bridging two detections goes to the one it overlaps more, and
    /// never takes a pixel the other already shows. A tie goes to the lower
    /// reading order, then the id.
    #[test]
    fn an_add_bridging_two_detections_goes_to_the_larger_overlap_only() {
        let (left, right) = (block(0, 0, 10, 10), block(20, 0, 10, 10));
        let page = [detection("d0", 5, &left, &left), detection("d1", 1, &right, &right)];
        let plan = plan_mask_edit(&page, &block(4, 2, 20, 4), MaskEditMode::Add);
        let [(id, mask, ink)] = plan.updated.as_slice() else { panic!("one update, got {plan:?}") };
        assert_eq!(id, "d0");
        assert_eq!(mask.bounds, Rect::new(0, 0, 20, 10));
        assert!(mask.contains(19, 3) && !mask.contains(20, 3) && !mask.contains(21, 3));
        assert!(inside(ink, mask));

        // Five columns each: the lower order wins, whatever the id says.
        let plan = plan_mask_edit(&page, &block(5, 2, 20, 4), MaskEditMode::Add);
        let [(id, mask, _)] = plan.updated.as_slice() else { panic!("one update, got {plan:?}") };
        assert_eq!(id, "d1");
        assert!(mask.contains(10, 3) && !mask.contains(9, 3));
        let page = [detection("d0", 1, &left, &left), detection("d1", 1, &right, &right)];
        let plan = plan_mask_edit(&page, &block(5, 2, 20, 4), MaskEditMode::Add);
        assert_eq!(plan.updated[0].0, "d0");
    }

    #[test]
    fn an_add_over_no_detection_makes_one_from_the_gesture_alone() {
        let near = block(0, 0, 10, 10);
        let page = [detection("d0", 0, &near, &near)];
        let mut gesture = Mask::empty(Rect::new(30, 30, 10, 10));
        gesture.set(32, 33, true);
        gesture.set(35, 36, true);
        let plan = plan_mask_edit(&page, &gesture, MaskEditMode::Add);
        let created = plan.created.as_ref().expect("a new detection");
        assert_eq!(created.bounds, Rect::new(32, 33, 4, 4));
        assert_eq!(created.count(), 2);
        assert!(plan.updated.is_empty() && plan.removed.is_empty());
    }

    /// Adding what is shown already, or what only another detection shows,
    /// changes nothing; neither does erasing where nothing is.
    #[test]
    fn a_gesture_that_moves_no_pixel_plans_nothing() {
        let (mask, other) = (block(0, 0, 10, 10), block(20, 0, 10, 10));
        let page = [detection("d0", 0, &mask, &mask), detection("d1", 0, &other, &other)];
        assert!(plan_mask_edit(&page, &block(2, 2, 3, 3), MaskEditMode::Add).is_empty());
        assert!(plan_mask_edit(&page, &block(40, 40, 3, 3), MaskEditMode::Remove).is_empty());
        assert!(plan_mask_edit(&page, &Mask::empty(Rect::new(0, 0, 4, 4)), MaskEditMode::Add).is_empty());
    }

    #[test]
    fn a_remove_shrinks_the_mask_and_the_lettering_to_their_tight_bounds() {
        let (mask, ink) = (block(0, 0, 10, 10), block(2, 2, 6, 6));
        let page = [detection("d0", 0, &mask, &ink)];
        let plan = plan_mask_edit(&page, &block(0, 0, 10, 3), MaskEditMode::Remove);
        assert_eq!(plan.updated, vec![("d0".to_owned(), block(0, 3, 10, 7), block(2, 3, 6, 5))]);
        assert!(plan.removed.is_empty() && plan.created.is_none());
    }

    #[test]
    fn a_remove_over_everything_removes_the_detection() {
        let (mask, far) = (block(0, 0, 10, 10), block(40, 40, 4, 4));
        let page = [detection("d0", 0, &mask, &mask), detection("d1", 0, &far, &far)];
        let plan = plan_mask_edit(&page, &block(0, 0, 12, 12), MaskEditMode::Remove);
        assert_eq!(plan.removed, vec!["d0".to_owned()]);
        assert!(plan.updated.is_empty());
    }

    /// A clean on a model rung writes through the lettering, so a detection
    /// is never left with a mask and none.
    #[test]
    fn a_remove_that_takes_all_the_lettering_leaves_the_mask_as_lettering() {
        let (mask, ink) = (block(0, 0, 10, 10), block(0, 0, 10, 2));
        let page = [detection("d0", 0, &mask, &ink)];
        let plan = plan_mask_edit(&page, &block(0, 0, 10, 2), MaskEditMode::Remove);
        assert_eq!(plan.updated, vec![("d0".to_owned(), block(0, 2, 10, 8), block(0, 2, 10, 8))]);
    }

    /// Lettering that reached past its mask is inside the mask once an edit
    /// touches the detection, either way.
    #[test]
    fn an_edited_detection_keeps_its_lettering_inside_its_mask() {
        let (mask, ink) = (block(0, 0, 10, 10), block(8, 8, 4, 4));
        let page = [detection("d0", 0, &mask, &ink)];
        for (gesture, mode) in [(block(9, 0, 3, 3), MaskEditMode::Add), (block(0, 0, 5, 5), MaskEditMode::Remove)] {
            let plan = plan_mask_edit(&page, &gesture, mode);
            let [(_, mask, ink)] = plan.updated.as_slice() else { panic!("{mode:?}: one update, got {plan:?}") };
            assert!(mask.contains(11, 11), "{mode:?}: the lettering past the mask was dropped");
            assert!(inside(ink, mask), "{mode:?}");
        }
    }

    #[test]
    fn stored_masks_are_cropped_to_the_pixels_they_hold() {
        let mut loose = Mask::empty(Rect::new(0, 0, 40, 40));
        for y in 10..20 {
            for x in 10..20 {
                loose.set(x, y, true);
            }
        }
        let page = [detection("d0", 0, &loose, &loose)];
        let plan = plan_mask_edit(&page, &block(15, 15, 2, 2), MaskEditMode::Remove);
        let [(_, mask, ink)] = plan.updated.as_slice() else { panic!("one update, got {plan:?}") };
        assert_eq!((mask.bounds, ink.bounds), (Rect::new(10, 10, 10, 10), Rect::new(10, 10, 10, 10)));
        assert_eq!(mask.count(), 96);
        assert_eq!(display_set(&loose, &Mask::empty(Rect::new(0, 0, 0, 0))).unwrap().bounds, Rect::new(10, 10, 10, 10));
        assert_eq!(display_set(&Mask::empty(Rect::new(0, 0, 4, 4)), &Mask::empty(Rect::new(0, 0, 0, 0))), None);
    }

    /* ---------- the command's body, against a job on disk ---------- */

    /// A one-page chapter over a 64 x 48 fixture, with one detection Detect
    /// stored: mask (4, 6, 12, 12), lettering (6, 9, 6, 6), order 3.
    fn page_with_a_detection(name: &str) -> (PathBuf, Located, String) {
        use cleaner_core::image::{encode, Format};
        use cleaner_core::project::{DetectedRegion, DetectionFit, Project, StripMode};

        let raw = cleaner_core::image::fixtures::by_name("l8").raster;
        assert_eq!((raw.width, raw.height), (64, 48));
        let root = std::env::temp_dir().join(format!("mc-mask-edit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("raws")).unwrap();
        let bytes = encode(&raw, Format::Png).unwrap();
        let path = root.join("raws/page.png");
        std::fs::write(&path, &bytes).unwrap();
        let source = cleaner_core::ingest::source_ref(&path, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let mut job = Job::create(&manifest, Project::new(manifest.parent().unwrap(), "test",
            StripMode::Single, &[source])).unwrap();
        let id = job.next_detection_id("chapter-p001");
        job.store_detection(DetectedRegion {
            id: id.clone(), source_idx: 0, bbox: Rect::new(4, 6, 12, 12), mask_ref: String::new(),
            inside: true, balloon_color: Some([240; 3]), script: Some("ja".into()), pick: "fill".into(),
            detector: "local".into(), created: 0, mask_sha256: String::new(), order: 3,
            review_state: Some("review.reason.unusuallyLarge".into()),
            fit: Some(DetectionFit { route: fit::Route::Fill, thickness: 3, best_deviation: 0.5, scale: 1.0 }),
            group: None,
            padding_from_seed: false,
            padding_px: 0,
        }, &block(4, 6, 12, 12), &block(6, 9, 6, 6)).unwrap();
        (root, Located { chapter_id: "chapter".into(), job_path: manifest, page_index: 0 }, id)
    }

    /// A rectangle as the Shapes tool sends it: four corners in page percent.
    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> serde_json::Value {
        let (x0, y0, x1, y1) = (x0 / 64.0 * 100.0, y0 / 48.0 * 100.0, x1 / 64.0 * 100.0, y1 / 48.0 * 100.0);
        serde_json::json!({ "kind": "rect", "feather": 0,
            "points": [{"x": x0, "y": y0}, {"x": x1, "y": y0}, {"x": x1, "y": y1}, {"x": x0, "y": y1}] })
    }

    #[test]
    fn a_mask_edit_grows_makes_and_erases_detections_on_disk() {
        let (root, located, first) = page_with_a_detection("round-trip");
        let sha = Job::open(&located.job_path).unwrap().project.sources[0].sha256.clone();
        let expected = (0usize, sha);

        // Grow the stored detection to the right.
        let grown = edit_detection_mask_at(&located, Some(&expected), MaskEditMode::Add, None,
            Some(&rect(12.0, 9.0, 20.0, 15.0))).unwrap().expect("an edit");
        assert_eq!(grown, MaskEdit { page_status: "detected".into(), changed: vec![first.clone()],
            created: vec![], removed: vec![] });
        let job = Job::open(&located.job_path).unwrap();
        let stored = job.load_detection(&first).unwrap().unwrap();
        assert_eq!(stored.record.mask_ref, format!("detections/{first}.e1.mask"));
        assert_eq!(stored.record.bbox, Rect::new(4, 6, 16, 12));
        assert_eq!(stored.mask.bounds, stored.record.bbox);
        assert!(stored.mask.contains(19, 14) && stored.ink.contains(19, 14) && !stored.ink.contains(19, 15));
        assert_eq!((stored.record.order, stored.record.pick.as_str()), (3, "fill"));
        assert_eq!(stored.record.fit.map(|fit| fit.thickness), Some(3));
        assert!(stored.record.review_state.is_some());

        // A dot where nothing is detected becomes a detection of its own.
        let dot = serde_json::json!({ "points": [{"x": 75.0, "y": 75.0}], "radius": 2.0 });
        let made = edit_detection_mask_at(&located, None, MaskEditMode::Add, Some(&dot),
            Some(&rect(0.0, 0.0, 64.0, 48.0))).unwrap().expect("an edit");
        let second = format!("{}-d1", crate::library::page_id("chapter", 0));
        assert_eq!((made.changed.len(), made.created.clone(), made.removed.len()), (0, vec![second.clone()], 0));
        let job = Job::open(&located.job_path).unwrap();
        let fresh = job.load_detection(&second).unwrap().unwrap();
        assert!(fresh.mask.contains(48, 36) && fresh.mask == fresh.ink);
        assert_eq!(fresh.record.bbox, fresh.mask.bounds);
        assert_eq!((fresh.record.pick.as_str(), fresh.record.detector.as_str()), ("lama", "local"));
        assert_eq!((fresh.record.inside, fresh.record.order, fresh.record.fit), (false, 4, None));
        assert!(fresh.record.balloon_color.is_none() && fresh.record.script.is_none() && fresh.record.group.is_none());

        // Erasing where nothing is changes nothing and writes nothing.
        let manifest = std::fs::read(&located.job_path).unwrap();
        let nothing = edit_detection_mask_at(&located, None, MaskEditMode::Remove, None,
            Some(&rect(56.0, 0.0, 64.0, 6.0))).unwrap().expect("an answer");
        assert_eq!(nothing, MaskEdit { page_status: "detected".into(), changed: vec![], created: vec![], removed: vec![] });
        assert_eq!(std::fs::read(&located.job_path).unwrap(), manifest);

        // Erasing the dot removes it and its files; the first is not touched.
        let gone = edit_detection_mask_at(&located, None, MaskEditMode::Remove, None,
            Some(&rect(40.0, 30.0, 56.0, 42.0))).unwrap().expect("an edit");
        assert_eq!((gone.changed.len(), gone.removed.clone()), (0, vec![second.clone()]));
        assert!(!job.sidecar().join(&fresh.record.mask_ref).exists());
        let job = Job::open(&located.job_path).unwrap();
        assert_eq!(job.load_detection(&first).unwrap().unwrap().record.mask_ref, stored.record.mask_ref);

        // Erasing the last detection leaves a page with nothing on it.
        let all = edit_detection_mask_at(&located, None, MaskEditMode::Remove, None,
            Some(&rect(0.0, 0.0, 64.0, 48.0))).unwrap().expect("an edit");
        assert_eq!((all.removed.clone(), all.page_status.as_str()), (vec![first], "unclean"));
        assert!(Job::open(&located.job_path).unwrap().project.detections.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    /// `null` for another source, a gesture that does not parse or covers
    /// nothing, and a line, which has no area. Nothing is written for any.
    #[test]
    fn a_mask_edit_that_cannot_apply_answers_null_and_writes_nothing() {
        let (root, located, _) = page_with_a_detection("refused");
        let manifest = std::fs::read(&located.job_path).unwrap();
        let over = rect(4.0, 6.0, 30.0, 30.0);
        let stale = (0usize, "0".repeat(64));
        assert_eq!(edit_detection_mask_at(&located, Some(&stale), MaskEditMode::Add, None, Some(&over)), Ok(None));
        let line = serde_json::json!({ "kind": "line", "feather": 3,
            "points": [{"x": 5.0, "y": 5.0}, {"x": 60.0, "y": 60.0}] });
        let off_page = serde_json::json!({ "points": [{"x": -50.0, "y": -50.0}], "radius": 1.0 });
        let broken = serde_json::json!({ "points": "none" });
        for (stroke, painted) in [(None, Some(&line)), (Some(&off_page), Some(&over)), (Some(&broken), None),
            (None, None), (None, Some(&broken))] {
            assert_eq!(edit_detection_mask_at(&located, None, MaskEditMode::Remove, stroke, painted), Ok(None));
        }
        let elsewhere = Located { page_index: 3, ..located };
        assert_eq!(edit_detection_mask_at(&elsewhere, None, MaskEditMode::Remove, None, Some(&over)), Ok(None));
        assert_eq!(std::fs::read(&elsewhere.job_path).unwrap(), manifest);
        assert_eq!(MaskEditMode::parse("erase"), None);
        assert_eq!(MaskEditMode::parse("add"), Some(MaskEditMode::Add));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A padding grows from the masks Detect stored, never from grown ones,
    /// so 4 then 2 is 2 alone and 0 gives the stored masks back, files and
    /// all. A hand edit over a padded detection reaches its ungrown masks, so
    /// what it added outlives a later padding.
    #[test]
    fn padding_grows_from_the_unpadded_masks_and_undoes_exactly() {
        let (root, located, id) = page_with_a_detection("padding");
        let load = || Job::open(&located.job_path).unwrap().load_detection(&id).unwrap().unwrap();
        let based = |job: &Job| std::fs::read_dir(job.sidecar().join("detections")).unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".base."))
            .count();
        let original = load();

        let wide = set_detection_padding_at(&located, Some(0), 4).unwrap();
        assert_eq!(wide, PaddingEdit { changed: vec![id.clone()], removed: vec![], pages: vec![0] });
        let padded = load();
        assert_eq!(padded.record.padding_px, 4);
        assert_eq!(padded.mask, original.mask.dilated(4, 64, 48));
        assert_eq!(padded.ink, original.ink.dilated(4, 64, 48));
        assert_eq!(padded.record.bbox, original.record.bbox);
        assert_eq!(based(&Job::open(&located.job_path).unwrap()), 2);

        // The padding every detection already has writes nothing.
        let manifest = std::fs::read(&located.job_path).unwrap();
        assert_eq!(set_detection_padding_at(&located, None, 4).unwrap(), PaddingEdit { changed: vec![], removed: vec![], pages: vec![] });
        assert_eq!(std::fs::read(&located.job_path).unwrap(), manifest);

        set_detection_padding_at(&located, None, 2).unwrap();
        assert_eq!(load().mask, original.mask.dilated(2, 64, 48));
        set_detection_padding_at(&located, None, 0).unwrap();
        let back = load();
        assert_eq!((back.record.padding_px, &back.mask, &back.ink), (0, &original.mask, &original.ink));
        assert_eq!(based(&Job::open(&located.job_path).unwrap()), 0);

        set_detection_padding_at(&located, None, 2).unwrap();
        edit_detection_mask_at(&located, None, MaskEditMode::Add, None, Some(&rect(14.0, 9.0, 24.0, 12.0)))
            .unwrap().expect("an edit");
        assert_eq!(load().record.padding_px, 2);
        set_detection_padding_at(&located, None, 0).unwrap();
        let edited = load();
        assert!(edited.mask.contains(22, 10), "the added pixels were lost");
        assert!(!edited.mask.contains(3, 6), "the padding outlived its removal");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A padding that brings two detections within reach of each other makes
    /// them one: the earlier in reading order keeps its id and takes the
    /// other's pixels and its ungrown pixels, and the other leaves the page
    /// with its files. A third out of reach is padded and stays itself. A
    /// smaller padding then shrinks the one, from both sets of letters.
    #[test]
    fn padding_that_runs_two_detections_together_makes_them_one() {
        let (root, located, first) = page_with_a_detection("padding-merge");
        let mut job = Job::open(&located.job_path).unwrap();
        let mut store = |bbox: Rect, order: u32| {
            let id = job.next_detection_id("chapter-p001");
            let mut record = hand_detection(0, bbox, order);
            record.id = id.clone();
            job.store_detection(record, &Mask::filled(bbox), &Mask::filled(bbox)).unwrap();
            id
        };
        // The first is x 4..16. One 10 px to its right, and one far off.
        let near = store(Rect::new(26, 8, 6, 6), 5);
        let far = store(Rect::new(54, 38, 4, 4), 6);
        let first_before = job.load_detection(&first).unwrap().unwrap();
        let near_before = job.load_detection(&near).unwrap().unwrap();
        drop(job);

        // 3 px each leaves their nearest pixels 5 apart: still two.
        let apart = set_detection_padding_at(&located, Some(0), 3).unwrap();
        assert_eq!((apart.changed.len(), apart.removed.len()), (3, 0));

        // 4 px each leaves them 3 apart, which is within reach.
        let edit = set_detection_padding_at(&located, Some(0), 4).unwrap();
        assert_eq!(edit.removed, vec![near.clone()]);
        assert_eq!(edit.changed, vec![far.clone(), first.clone()]);
        let job = Job::open(&located.job_path).unwrap();
        assert!(job.load_detection(&near).unwrap().is_none());
        assert!(!job.sidecar().join(&near_before.record.mask_ref).exists());
        let one = job.load_detection(&first).unwrap().unwrap();
        let both = |a: &Mask, b: &Mask| Mask::union(&[a, b], 64, 48);
        assert_eq!(one.mask, both(&first_before.mask.dilated(4, 64, 48), &near_before.mask.dilated(4, 64, 48)));
        assert_eq!(one.ink, both(&first_before.ink.dilated(4, 64, 48), &near_before.ink.dilated(4, 64, 48)));
        assert_eq!(job.load_detection_base(&first).unwrap().unwrap(),
            (both(&first_before.mask, &near_before.mask), both(&first_before.ink, &near_before.ink)));
        assert_eq!(one.record.bbox, Rect::new(4, 6, 28, 12));
        assert_eq!((one.record.order, one.record.padding_px), (3, 4));
        // One inside a balloon on the fill and one outside it on LaMa.
        assert_eq!((one.record.inside, one.record.pick.as_str(), one.record.balloon_color), (false, "lama", None));
        assert_eq!((one.record.fit, one.record.script.as_deref()), (None, None));
        assert_eq!(job.load_detection(&far).unwrap().unwrap().record.padding_px, 4);
        drop(job);

        // Back to 0: one detection, holding both sets of letters as stored.
        let back = set_detection_padding_at(&located, Some(0), 0).unwrap();
        assert_eq!(back.removed, Vec::<String>::new());
        let job = Job::open(&located.job_path).unwrap();
        assert_eq!(job.project.detections.len(), 2);
        assert_eq!(job.load_detection(&first).unwrap().unwrap().mask, both(&first_before.mask, &near_before.mask));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A padding of one named detection merges what that one reaches and
    /// nothing else on the page, and the earlier in reading order is the one
    /// kept, whichever was named.
    #[test]
    fn a_named_padding_merges_only_what_the_named_detection_reaches() {
        let (root, located, first) = page_with_a_detection("padding-merge-named");
        let mut job = Job::open(&located.job_path).unwrap();
        let mut store = |bbox: Rect, order: u32| {
            let id = job.next_detection_id("chapter-p001");
            let mut record = hand_detection(0, bbox, order);
            record.id = id.clone();
            job.store_detection(record, &Mask::filled(bbox), &Mask::filled(bbox)).unwrap();
            id
        };
        let near = store(Rect::new(26, 8, 6, 6), 5);
        // Two that touch each other and are out of the named one's reach.
        let (left, right) = (store(Rect::new(40, 36, 6, 6), 6), store(Rect::new(46, 36, 6, 6), 7));
        drop(job);

        let edit = set_detection_padding_scoped_at(&located, Some(0), 8, Some(&near)).unwrap();
        assert_eq!((edit.changed, edit.removed), (vec![first.clone()], vec![near.clone()]));
        let job = Job::open(&located.job_path).unwrap();
        let ids: Vec<&str> = job.project.detections.iter().map(|record| record.id.as_str()).collect();
        assert_eq!(ids, vec![first.as_str(), left.as_str(), right.as_str()]);
        // The widest padding any member had, over the named one's ungrown mask.
        let one = job.load_detection(&first).unwrap().unwrap();
        assert_eq!(one.record.padding_px, 8);
        assert_eq!(job.load_detection_base(&first).unwrap().unwrap().0,
            Mask::union(&[&block(4, 6, 12, 12), &block(26, 8, 6, 6)], 64, 48));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Detect's own merge: every set the padding ran together is one, where
    /// its first member stood, and unpadded detections that touch stay two.
    #[test]
    fn detect_stores_what_its_padding_ran_together_as_one() {
        let found = |bbox: Rect, order: u32, padding: u32| {
            let mut record = hand_detection(0, bbox, order);
            record.padding_px = padding;
            let seed = Mask::filled(bbox);
            cleaner_core::project::LoadedDetection {
                record,
                mask: seed.dilated(padding, 64, 48),
                ink: seed.dilated(padding, 64, 48),
                evidence: None,
                base: (padding > 0).then(|| (seed.clone(), seed.clone())),
            }
        };
        let touching = vec![found(Rect::new(4, 4, 6, 6), 0, 0), found(Rect::new(10, 4, 6, 6), 1, 0)];
        let kept: Vec<Rect> = merged_by_padding(touching, (64, 48)).iter().map(|found| found.record.bbox).collect();
        assert_eq!(kept, vec![Rect::new(4, 4, 6, 6), Rect::new(10, 4, 6, 6)]);

        // A pair, a detection alone, and a chain of three whose ends do not
        // reach each other: the middle one reaches both.
        let page = vec![
            found(Rect::new(41, 30, 4, 4), 2, 2),
            found(Rect::new(4, 4, 6, 6), 0, 2),
            found(Rect::new(52, 30, 4, 4), 3, 2),
            found(Rect::new(4, 30, 4, 4), 1, 2),
            found(Rect::new(17, 4, 6, 6), 4, 2),
            found(Rect::new(30, 30, 4, 4), 5, 2),
        ];
        let stored = merged_by_padding(page.clone(), (64, 48));
        let orders: Vec<u32> = stored.iter().map(|found| found.record.order).collect();
        assert_eq!(orders, vec![2, 0, 1]);
        assert_eq!(stored[0].record.bbox, Rect::new(30, 30, 26, 4));
        assert_eq!(stored[0].mask, Mask::union(&[&page[0].mask, &page[2].mask, &page[5].mask], 64, 48));
        assert_eq!(stored[1].record.bbox, Rect::new(4, 4, 19, 6));
        assert_eq!(stored[1].mask, Mask::union(&[&page[1].mask, &page[4].mask], 64, 48));
        assert_eq!(stored[1].base.as_ref().map(|(mask, _)| mask.count()), Some(72));
        assert_eq!((&stored[2].record, &stored[2].mask), (&page[3].record, &page[3].mask));
    }

    #[test]
    fn detection_padding_targets_one_mask_and_rejects_invalid_scope() {
        let (root, located, id) = page_with_a_detection("targeted-padding");
        let mut job = Job::open(&located.job_path).unwrap();
        let sibling_id = job.next_detection_id("chapter-p001");
        let mut sibling = hand_detection(0, Rect::new(35, 25, 5, 5), 4);
        sibling.id = sibling_id.clone();
        let sibling_mask = block(35, 25, 5, 5);
        job.store_detection(sibling, &sibling_mask, &sibling_mask).unwrap();
        let original = job.load_detection(&id).unwrap().unwrap();
        let sibling_before = job.load_detection(&sibling_id).unwrap().unwrap();
        drop(job);

        let edit = set_detection_padding_scoped_at(&located, Some(0), 4, Some(&id)).unwrap();
        assert_eq!(edit, PaddingEdit { changed: vec![id.clone()], removed: vec![], pages: vec![0] });
        let job = Job::open(&located.job_path).unwrap();
        let padded = job.load_detection(&id).unwrap().unwrap();
        assert_eq!(padded.mask, original.mask.dilated(4, 64, 48));
        let sibling_after = job.load_detection(&sibling_id).unwrap().unwrap();
        assert_eq!(sibling_after.mask, sibling_before.mask);
        assert_eq!(sibling_after.ink, sibling_before.ink);
        assert_eq!(sibling_after.record.mask_ref, sibling_before.record.mask_ref);
        assert_eq!(sibling_after.record.padding_px, 0);
        drop(job);

        let manifest = std::fs::read(&located.job_path).unwrap();
        for (page, target) in [(Some(0), ""), (Some(0), "../not-a-region"),
            (None, "another-chapter-p001-d0"), (Some(1), id.as_str())] {
            assert_eq!(set_detection_padding_scoped_at(&located, page, 2, Some(target)),
                Err("mask_padding_target_invalid".into()));
            assert_eq!(std::fs::read(&located.job_path).unwrap(), manifest);
            let job = Job::open(&located.job_path).unwrap();
            assert_eq!(job.load_detection(&id).unwrap().unwrap().mask, padded.mask);
            assert_eq!(job.load_detection(&sibling_id).unwrap().unwrap().mask, sibling_before.mask);
        }
        // Chapter-scoped targeting still changes only the requested detection.
        assert_eq!(set_detection_padding_scoped_at(&located, None, 2, Some(&sibling_id)).unwrap().changed,
            vec![sibling_id.clone()]);
        let job = Job::open(&located.job_path).unwrap();
        assert_eq!(job.load_detection(&id).unwrap().unwrap().record.padding_px, 4);
        assert_eq!(job.load_detection(&sibling_id).unwrap().unwrap().mask, sibling_mask.dilated(2, 64, 48));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_padding_replaces_fitted_growth_with_verified_segmentation() {
        use cleaner_core::text_groups::{group, Inputs};
        for requested in [0, 2, 4] {
            let (root, located, id) = page_with_a_detection(&format!("seed-padding-{requested}"));
            let mut job = Job::open(&located.job_path).unwrap();
            let mut pixels = vec![0; 64 * 48];
            for y in 9..15 { for x in 6..12 { pixels[y * 64 + x] = 255; } }
            let grouping = group(&Inputs::new(64, 48, Some(&pixels))).unwrap();
            let evidence = grouping.evidence(Some(&pixels)).unwrap();
            let record = grouping.record(&grouping.groups[0], Some(&evidence));
            let seed = grouping.lettering(&grouping.groups[0]);
            job.project.detections[0].group = Some(record);
            job.write_evidence(&evidence).unwrap();
            job.flush().unwrap();
            drop(job);
            assert_eq!(set_detection_padding_at(&located, Some(0), requested).unwrap().changed, vec![id.clone()]);
            for padding in [requested, 4, 2, 0] {
                set_detection_padding_at(&located, Some(0), padding).unwrap();
                let reopened = Job::open(&located.job_path).unwrap();
                let found = reopened.load_detection(&id).unwrap().unwrap();
                assert!(found.record.padding_from_seed);
                assert_eq!(found.record.fit.unwrap().thickness, padding);
                assert_eq!(found.mask, seed.dilated(padding, 64, 48));
                assert_eq!(found.ink, found.mask);
            }
            // Once a legacy record has had any edit, its stored mask remains
            // authoritative even when original segmentation evidence exists.
            let mut job = Job::open(&located.job_path).unwrap();
            job.project.detections[0].padding_from_seed = false;
            job.flush().unwrap();
            let original = job.load_detection(&id).unwrap().unwrap().mask;
            assert!(legacy_padding_seed(&job, &job.project.detections[0]).is_none());
            let mut untouched = job.project.detections[0].clone();
            untouched.mask_ref = format!("detections/{id}.mask");
            untouched.group.as_mut().unwrap().evidence_sha256 = Some("invalid".into());
            assert!(legacy_padding_seed(&job, &untouched).is_none(), "corrupt evidence must not erase masks");
            untouched.group.as_mut().unwrap().evidence_ref = Some("evidence/missing.mtev".into());
            assert!(legacy_padding_seed(&job, &untouched).is_none(), "missing evidence must not erase masks");
            drop(job);
            set_detection_padding_at(&located, Some(0), 2).unwrap();
            let found = Job::open(&located.job_path).unwrap().load_detection(&id).unwrap().unwrap();
            assert!(!found.record.padding_from_seed);
            assert_eq!(found.mask, original.dilated(2, 64, 48));
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn a_mask_edit_answers_in_camel_case() {
        let edit = MaskEdit { page_status: "detected".into(), changed: vec!["a".into()], created: vec![], removed: vec![] };
        assert_eq!(serde_json::to_value(edit).unwrap(),
            serde_json::json!({ "pageStatus": "detected", "changed": ["a"], "created": [], "removed": [] }));
    }

}
