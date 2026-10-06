//! The library: projects, chapters, pages and regions, over the job manifest.
//!
//! The seam asks for a tree - `listProjects` returns the whole tree:
//! projects, chapters, pages, regions. The core has a *job*
//! ([`cleaner_core::project::Job`], the `.mtclean` manifest) and no notion
//! of a project or a chapter at all. This module is the model that sits
//! between, and the decisions it turns on are all one question: **what does a
//! manifest already know, and what can only a library know?**
//!
//! ## A chapter is a job
//!
//! One `.mtclean` file, one chapter. Nothing else lines up: a manifest holds
//! one `sources` list, one `strip` order and one set of patches, which is
//! exactly what a chapter is, and a project is a *label over several of them*
//! plus the three settings that are fixed for the project's life. So a
//! project has no manifest of its own, and never
//! needs one.
//!
//! ## What the index holds, and what it must not
//!
//! `index.json` under the app's data directory holds **only what no manifest
//! can**: the grouping of chapters into projects, the names a user typed, the
//! source folder, the reading direction, the page mode, and when each was last
//! opened. Everything a chapter *contains* - its pages, its regions, its
//! masks, what was refused on the way in, whether a run stopped part-way - is
//! read out of that chapter's manifest on every call.
//!
//! The line is not stylistic. The core keeps `settings` opaque for a reason:
//! a second definition of a shape is a second thing to disagree with the
//! first. An index that cached page counts,
//! or region outcomes, or "is this chapter cleaned", would be exactly that
//! second definition, and it would be the one that goes stale - the manifest is
//! rewritten by `Job::complete_region` after every region and the index is not.
//! So the index is not a cache and there is no code here that would let it
//! become one.
//!
//! The two things the manifest could not say, it now says: `input_report` and
//! `interrupted_at` were added to [`cleaner_core::project::Project`] rather
//! than kept here, because both are facts about a job.
//!
//! ## A project's folder and a chapter's folder are two facts
//!
//! Both are in the index, and neither stands in for the other. A project's
//! source folder is where its scans live; a chapter's is the folder its own
//! pages were read from, which is usually a subfolder of the project's and is
//! sometimes somewhere else entirely.
//!
//! The seam once carried only the first of the two, and `createChapter`
//! inferred the second from the chapter's name - with the consequence that two
//! chapters which both fell back to the project's folder held the same files
//! under two numbers. `createChapter` now takes the folder. The inference
//! survives as a
//! *default* for the case it was always right for - the first chapter of a
//! freshly pointed-at scan folder - and [`Library::inferred_source_dir`] refuses
//! to reach the same answer twice, so the failure is now a sentence the user
//! reads rather than a duplicate they discover later.
//!
//! Nothing re-derives it afterwards. What a chapter reads is settled by its
//! manifest's `sources`, written once at ingest, and `IndexChapter.source_path`
//! is a record of the folder they came from rather than an instruction to go
//! back to it. That is why a library written by an earlier build needs no
//! migration: its chapters have no recorded folder, their manifests are exactly
//! as they were, and [`Library::chapter_source`] answers the question by reading
//! the manifest instead of guessing what the old rule would have chosen.
//!
//! ## Where the jobs live
//!
//! `<app data>/library/<project id>/<chapter id>.mtclean`, with the sidecar
//! buffer directory beside it - not next to the user's scans. Three reasons,
//! in the order they decided it:
//!
//! - **`deleteProject` promises the files on disk are untouched**, and the
//!   interface's own copy says "the pages in {path} are left exactly as they
//!   are". A job's sidecar directory runs to gigabytes on a 200-page chapter.
//!   If it sat in the user's scan
//!   folder, removing a project from the library would either delete their work
//!   or strand gigabytes beside their scans with nothing left that names it.
//!   Under the library root it is the library's own storage, and it is
//!   reachable by the launch sweep.
//! - **"Written beside the output" presumes an output.** There is none
//!   until `exportChapter` is called and the user names a destination, which is
//!   also the only moment `Job::output_refusal` can be evaluated. A chapter
//!   created this morning and never exported has no output directory to be
//!   beside.
//! - **The source folder is read-only by promise** - "Its files are never
//!   written to", says the New project dialog.
//!
//! The cost, stated: §1's "paths are relative … so a job survives being moved"
//! is weakened, because `converted_from` from an app-data manifest to a source
//! in `~/scans` is a long `..` chain rather than a sibling. Moving the library
//! root moves every manifest in it together, which is the case the relative
//! form actually buys.
//!
//! ## A chapter owns its pages
//!
//! `createChapter` does not only *read* the scan folder - it takes a copy of
//! every accepted page into the job's own `<chapter>.mtclean.d/pages/` and
//! points `rel_path` at that ([`Library::write_job`]). The user's folder is
//! recorded in `converted_from`
//! and never opened again, so the scan folder can be moved or deleted and the
//! chapter still opens, cleans, resumes and exports.
//!
//! Two things follow, and both are why this is a whole-application decision
//! rather than an ingest one:
//!
//! - **The library's copy can be the only copy.** `deleteProject` still leaves
//!   the user's scan folder exactly as it is, and that sentence is now the
//!   smaller half of the truth: what it removes is a copy the user may have
//!   deleted their own original for. The confirm copy says so.
//! - **The user's folder is still the origin.** Every question about *where the
//!   chapter came from* - the export's `<input>_cleaned/` sibling, which files
//!   "output never overwrites input" must refuse, what
//!   [`Library::inferred_source_dir`] considers taken - reads `converted_from`
//!   through [`cleaner_core::project::Job::origin_path`], and none of them
//!   needs the file to still be there.
//!
//! The cost is a second copy of every page under the library root, which for a
//! 200-page colour chapter is on the order of a gigabyte. Import estimates
//! that copy and checks available space on the destination volume before it
//! writes the first page.
//!
//! ## When a job is missing or unreadable
//!
//! The chapter is still listed, with no pages. It is **not** dropped and it
//! does **not** fail the read, for two separate reasons: a chapter leaves the
//! library only when a user removes the project, never because of a filesystem
//! accident; and `createChapter` numbers the next chapter `max + 1`, so a
//! chapter that vanished from the listing would have its number handed out
//! again.
//!
//! What is lost is the ability to *say so*: the catalogue (the fixed
//! vocabulary) has
//! `notice.library.changeFailed` for a change that could not be saved and no
//! key at all for "this chapter's job file could not be read", so an unreadable
//! manifest is today indistinguishable from a chapter nobody has put pages in.
//! Inventing a key is a test failure, so this is recorded rather than papered
//! over.
//!
//! ## Pages are sources, and a refused file is not a page
//!
//! `ApiPage.index` is a position in `strip.order`, and `strip.order` holds
//! indices into `sources`. A file refused at ingest
//! never became a source, so it cannot sit in `strip.order` and therefore
//! cannot be a page: giving it a page index would make `runClean`'s
//! `pageIndex`, the longstrip split rows and `strip.order` itself disagree
//! about what page 9 is. Refusals surface where the seam already has a place
//! for them - `ApiChapter.inputReports`, which `openChapter` staggers onto the
//! notice stack.
//!
//! **For anything else addressing a page: `source_idx = strip.order[index]`.**
//! Today `strip.order` is `0..n` and the two are equal; they stop being equal
//! the moment a user reorders pages, so go through [`Library::resolve_page`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use cleaner_core::engines::render::CloudProvider;
use cleaner_core::ingest;
use cleaner_core::mask::Rect;
use cleaner_core::patch::{CloudRecord, Engine};
use cleaner_core::project::{
    buffers, sidecar_dir, DetectedRegion, Job, PatchRecord, Project, RegionUntouched, StoreError,
    StripMode,
};

/// The library's own directory under the app's data directory, and the index
/// inside it.
const LIBRARY_DIR: &str = "library";
const INDEX_FILE: &str = "index.json";

/// Where a page converted at ingest is written, inside the job's sidecar
/// directory. A sub-directory rather than the sidecar root so that a page file
/// can never collide with a mask or a patch buffer, which are named by region
/// id and live directly in it.
/// The job's own page directory, inside the sidecar: every page a chapter
/// holds, copied or converted there at ingest.
///
/// Named `pages` and not `converted` because it stopped being only about
/// conversion: a folder of PNGs fills it too. A chapter written by an earlier
/// build keeps its `converted/` directory and needs no migration - a manifest
/// stores the path it wrote, and nothing recomputes it.
const PAGES_DIR: &str = "pages";

/// The index format this build writes.
const INDEX_VERSION: u32 = 1;

/* ------------------------------------------------------------------ */
/* Errors                                                              */
/* ------------------------------------------------------------------ */

#[derive(Debug)]
pub enum LibraryError {
    /// The app's data directory could not be located, or the index could not
    /// be read or written.
    Index { path: PathBuf, detail: String },
    /// No project or chapter in the index has that id.
    Unknown { id: String },
    /// The chapter is in the index and has no job on disk - a chapter created
    /// before its source folder held anything.
    NoJob { chapter_id: String },
    /// The job is on disk and could not be opened.
    Manifest { path: PathBuf, detail: String },
    InsufficientSpace { required: u64, available: u64 },
    FreeSpaceUnavailable { path: PathBuf },
    /// A layer change its capabilities do not allow. Displayed as its
    /// catalogue key, which is what the interface names the refusal by.
    LayerRefused(cleaner_core::patch::LayerRefusal),
    /// Another process held the chapter, or the index, for longer than a
    /// command waits ([`crate::run::lock_job`]). Nothing was changed.
    Busy { path: PathBuf },
}

impl From<crate::run::LockError> for LibraryError {
    fn from(error: crate::run::LockError) -> LibraryError {
        match error {
            crate::run::LockError::Busy { path } | crate::run::LockError::Stopped { path } => {
                LibraryError::Busy { path }
            }
        }
    }
}

/// Which rule [`Library::set_layer_style`] judges a request by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerEdit {
    /// A deliberate edit: refused whole when the capabilities disallow it.
    Edit,
    /// History replay: restored as far as the capabilities allow.
    Restore,
}

impl LibraryError {
    /// The i18n key the seam reports this under, the way
    /// [`cleaner_core::project::OutputRefusal::reason_key`] and
    /// [`cleaner_core::gate::Verdict::reason_key`] do.
    ///
    /// Every arm answers the same key, and that is a statement about the
    /// catalogue rather than laziness. The catalogue is the fixed vocabulary
    /// and it has exactly one entry
    /// for a library operation that did not happen - *"That change could not be
    /// saved. The library is as it was."* A second key for "the job file could
    /// not be read" would be a key the core invented, which fails `npm test`.
    /// The match is written out so that the moment a key exists, the arm that
    /// wants it is already here.
    pub fn reason_key(&self) -> &'static str {
        match self {
            LibraryError::Index { .. } => "notice.library.changeFailed",
            LibraryError::Unknown { .. } => "notice.library.changeFailed",
            LibraryError::NoJob { .. } => "notice.library.changeFailed",
            LibraryError::Manifest { .. } => "notice.library.changeFailed",
            LibraryError::InsufficientSpace { .. } => "notice.library.ingestInsufficientSpace",
            LibraryError::FreeSpaceUnavailable { .. } => "notice.library.ingestSpaceUnknown",
            LibraryError::LayerRefused(refusal) => refusal.reason_key(),
            LibraryError::Busy { .. } => "notice.job.busy",
        }
    }
}

impl std::fmt::Display for LibraryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LibraryError::Index { path, detail } => write!(f, "{}: {detail}", path.display()),
            LibraryError::Unknown { id } => write!(f, "no such project or chapter: {id}"),
            LibraryError::NoJob { chapter_id } => {
                write!(f, "chapter {chapter_id} has no job on disk yet")
            }
            LibraryError::Manifest { path, detail } => write!(f, "{}: {detail}", path.display()),
            LibraryError::InsufficientSpace { required, available } => write!(f, "chapter import needs {required} bytes, but only {available} bytes are available"),
            LibraryError::FreeSpaceUnavailable { path } => write!(f, "could not check free space for chapter import at {}", path.display()),
            LibraryError::LayerRefused(refusal) => f.write_str(refusal.reason_key()),
            // The code first, as `LockError::Busy` says it, so the interface
            // finds it wherever the message goes.
            LibraryError::Busy { path } => {
                write!(f, "job_busy: another Manga Cleaner process is using {}", path.display())
            }
        }
    }
}

impl std::error::Error for LibraryError {}

/* ------------------------------------------------------------------ */
/* The index                                                           */
/* ------------------------------------------------------------------ */

/// Manga is right-to-left, and the direction is per project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadingDirection {
    #[default]
    Rtl,
    Ltr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexChapter {
    pub id: String,
    pub name: String,
    /// The number the user gave the chapter at creation, stored verbatim, and
    /// never renumbered - deleting Ch. 3 leaves a gap rather than turning
    /// Ch. 4 into Ch. 3 under a user who has already written the number down.
    /// `max + 1` is only the fallback for a caller that names no number, and is
    /// what the New chapter dialog starts its field at.
    pub number: u32,
    /// The folder this chapter's pages were read from, as `createChapter`
    /// resolved it - **recorded once, at creation, and never re-derived**.
    ///
    /// `None` means *unknown*, and it is what every chapter created before the
    /// seam carried a path has. It
    /// is not a licence to guess: what a chapter reads is settled by its
    /// manifest's `sources`, which were written at ingest and are not consulted
    /// again, so an absent record costs the library the ability to *say* where
    /// the files came from and nothing else. [`Library::chapter_source`]
    /// recovers the answer for display from the manifest rather than by
    /// re-running the inference, which is the one thing that could point a
    /// chapter at a different folder than the one it was built from.
    #[serde(default)]
    pub source_path: Option<PathBuf>,
    pub created: u64,
    pub last_opened: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexProject {
    pub id: String,
    pub name: String,
    /// Fixed for the project's life. Restated in each chapter's manifest as
    /// `strip.mode`, which is where the pipeline reads it; this copy is what
    /// makes it answerable before any chapter exists.
    pub mode: StripMode,
    pub source_path: Option<PathBuf>,
    #[serde(default)]
    pub reading_direction: ReadingDirection,
    pub created: u64,
    pub last_opened: u64,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub chapters: Vec<IndexChapter>,
    /// The cloud endpoint this project's first cloud consent was given for.
    /// Later requests to the same endpoint skip the question; each one still
    /// gets its own proposal and single-use grant. An older build drops the
    /// field on save, which only means the question is asked again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_consent: Option<CloudConsent>,
}

/// A project's standing cloud consent: which endpoint, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudConsent {
    pub provider: CloudProvider,
    pub profile_id: String,
    /// `compute_canonical_endpoint_fingerprint` of the endpoint consented to.
    /// A resume that moves the endpoint asks again.
    pub origin_fingerprint: String,
    /// Unix seconds.
    pub granted_at: u64,
    /// The rights and retention statements were ticked when this consent
    /// was given. Only such a consent stands: one recorded before the region
    /// dialog asked for them has none, and asks once more.
    #[serde(default)]
    pub statements_accepted: bool,
}

impl CloudConsent {
    pub fn covers(&self, provider: CloudProvider, profile_id: &str, origin_fingerprint: &str) -> bool {
        self.provider == provider
            && self.profile_id == profile_id
            && self.origin_fingerprint == origin_fingerprint
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    /// One counter for projects and chapters together, so a chapter id is
    /// unique across the whole library - [`resolve_chapter`] is given a chapter
    /// id and nothing else, and has to find the project from it.
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub projects: Vec<IndexProject>,
    /// The digest of the file this index was read from, `None` for a library
    /// with no index yet. [`Library::save`] refuses to write over a file that
    /// has moved since, the way [`Job::flush`] refuses a manifest.
    #[serde(skip)]
    seen: Option<String>,
}

impl Default for Index {
    fn default() -> Self {
        Index {
            version: INDEX_VERSION,
            next_id: 1,
            projects: Vec::new(),
            seen: None,
        }
    }
}

/* ------------------------------------------------------------------ */
/* The shapes the seam carries                                         */
/* ------------------------------------------------------------------ */

/// A translatable time, carried as data so the interface renders it in the
/// user's language.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RelativeTime {
    pub key: &'static str,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticeSpec {
    pub key: String,
    pub params: serde_json::Value,
    pub tone: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bbox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudOutcome {
    pub accepted: bool,
    pub rejection_cause: Option<&'static str>,
}

/// Validate and sanitize an identifier string exposed across the public Tauri seam.
///
/// Implements conservative lexical sanitization using a bounded symbolic allowlist:
/// allows ASCII alphanumeric characters plus `.`, `_`, `-`, `/` up to 128 characters,
/// and rejects control characters, whitespace, URLs, userinfo, and empty inputs.
///
/// NOTE: This provides conservative lexical sanitization and boundary enforcement,
/// not an exhaustive guarantee of cryptographic secret detection.
pub fn sanitize_identifier(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > 128 {
        return None;
    }
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    if !raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' || c == '/')
    {
        return None;
    }
    if raw.contains("//") || raw.starts_with('/') || raw.ends_with('/') {
        return None;
    }
    Some(raw.to_string())
}

/// Convert a persisted [`CloudRecord`] into a safe public representation across the seam.
///
/// Sanitizes newly added identifiers, rejecting raw URLs, credentials, control characters,
/// and length bounds (> 128 chars), while preserving legacy engine and model strings on disk.
pub fn sanitize_cloud_record(cloud: &CloudRecord) -> CloudRecord {
    CloudRecord {
        provider: sanitize_identifier(&cloud.provider).unwrap_or_else(|| "unknown".to_string()),
        profile_id: cloud.profile_id.as_deref().and_then(sanitize_identifier),
        job_id: cloud.job_id.as_deref().and_then(sanitize_identifier),
        request_id: sanitize_identifier(&cloud.request_id).unwrap_or_else(|| "unknown".to_string()),
        attempt_id: cloud.attempt_id.as_deref().and_then(sanitize_identifier),
        recipe_id: cloud.recipe_id.as_deref().and_then(sanitize_identifier),
        model: sanitize_identifier(&cloud.model).unwrap_or_else(|| "unknown".to_string()),
        model_revision: cloud
            .model_revision
            .as_deref()
            .and_then(sanitize_identifier),
        tier: cloud.tier.as_deref().and_then(sanitize_identifier),
        cost: cloud.cost,
        duration_ms: cloud.duration_ms,
    }
}

/// `Provenance` with one field changed: `created` crosses the seam as ISO 8601
/// and is epoch seconds in the core.
///
/// Every other name is snake_case verbatim, because the persisted form is what
/// gets written and
/// `src/lib/model/types.js` restates those names exactly. **This struct is the
/// one place the epoch-to-ISO conversion happens**; anything else that puts a
/// `Provenance` on the seam goes through [`ApiProvenance::of`] rather than
/// formatting a second time.
#[derive(Debug, Clone, Serialize)]
pub struct ApiProvenance {
    pub engine: Engine,
    pub engine_version: String,
    pub model_sha256: Option<String>,
    pub execution_provider: String,
    pub params_snapshot: serde_json::Value,
    pub mask_sha256: String,
    pub source_sha256: String,
    pub cloud: Option<CloudRecord>,
    pub created: String,
}

impl ApiProvenance {
    pub fn of(provenance: &cleaner_core::patch::Provenance) -> ApiProvenance {
        ApiProvenance {
            engine: provenance.engine,
            engine_version: provenance.engine_version.clone(),
            model_sha256: provenance.model_sha256.clone(),
            execution_provider: provenance.execution_provider.clone(),
            params_snapshot: provenance.params_snapshot.clone(),
            mask_sha256: provenance.mask_sha256.clone(),
            source_sha256: provenance.source_sha256.clone(),
            cloud: provenance.cloud.as_ref().map(sanitize_cloud_record),
            created: iso8601(provenance.created),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiMask {
    /// `<region id>-m<n>`. The shape is load-bearing: the seam addresses masks
    /// by id (`deleteMask`, `rerunMask`) and the region is recovered from it.
    pub id: String,
    /// Explicit geometry policy. Old manifest rows deserialize as legacy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geometry_policy: Option<cleaner_core::text_shape::GeometryPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_shape_plan_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_shape_patch_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_patch_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask_quality_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_support_sha256: Option<String>,
    pub region_id: String,
    pub sequence: u32,
    pub fill_mode: &'static str,
    pub elapsed_ms: u64,
    pub fitting_reconstructed: bool,
    pub dependency_review: Option<&'static str>,
    /// A review reason about the mask itself, as its `review.reason.*` key:
    /// grouping evidence against the lettering it was cleaned through
    /// (`crossesBalloon`, `maskMissingUnderBox`), or `unrecognized` for a
    /// stored key this build has no meaning for. See [`review_flags`].
    pub mask_review: Option<&'static str>,
    /// A non-mask-conditioned cloud render replaced a detection whose stored
    /// fit required inpainting. This asks for a visual check; it does not
    /// assert that the generated texture is wrong.
    pub generated_texture_review: bool,
    pub cloud_outcome: Option<CloudOutcome>,
    pub provenance: ApiProvenance,
    pub layer: cleaner_core::patch::LayerStyle,
    /// What the interface may offer on this layer, derived here from what
    /// produced it and enforced again by [`Library::set_layer_style`].
    pub capabilities: cleaner_core::patch::LayerCapabilities,
    /// The patch's untransformed bounds, in the same percent space as the
    /// region's `bbox` (which is the box it is *drawn* in). The pivot of every
    /// move and rotation is this box's centre plus the saved offset.
    pub source_bbox: Bbox,
    /// [`crate::tile::record_appearance`]: changes exactly when what this
    /// layer draws changes. `null` for a detection, which draws nothing.
    pub appearance: Option<String>,
    /// [`crate::tile::layer_appearance`]: changes exactly when the pixels of
    /// this patch's `layer` image do - never for opacity, which the interface
    /// draws. `null` for a detection.
    pub layer_key: Option<String>,
    /// The patch's place in the compositing stack: lower first, ties by id,
    /// as `cleaner_core::composite` stamps them. `null` for a detection.
    pub order: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRegion {
    pub id: String,
    pub page_id: String,
    pub source_sha: String,
    /// Percentages of the page, 0–100 - the region's own normalised space, not
    /// the manifest's pixels. See [`percent_of`].
    pub bbox: Bbox,
    pub source: &'static str,
    pub detected: bool,
    /// Current source-pixel padding for a stored detection's editable mask.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding_px: Option<u32>,
    pub outcome: &'static str,
    pub gate_skip_cause: Option<&'static str>,
    pub decline_reason: Option<String>,
    /// Set only on `outcome: "candidate"`: the grouping reason a text-group
    /// candidate was held under (`review.reason.unassignedMask`,
    /// `review.reason.isolatedMask`). A candidate is lettering no text box
    /// claimed, listed for the user to pick and never cleaned on its own. It
    /// is not a failure and not a problem, so it is in no review count.
    pub candidate_reason: Option<String>,
    /// A flag the manifest cannot carry, read from the cloud attempt journal
    /// ([`CLOUD_ATTENTION_KEYS`]): a committed cloud result for this region
    /// went missing, a result arrived and was never applied, or one could not
    /// be checked. On a detection or a layer. `None` otherwise.
    pub attention: Option<&'static str>,
    /// Set on a candidate only: whether grouping found it inside a balloon,
    /// which decides the engine a clean of it starts on (`None` on a row from
    /// before that was recorded, which starts outside).
    pub candidate_inside_bubble: Option<bool>,
    /// Set on a stored detection only: its balloon answer,
    /// [`DetectedRegion::inside`]. The interface draws the mask of text inside
    /// a speech bubble and of text outside one in two colours. `None` on
    /// every other row.
    pub inside_bubble: Option<bool>,
    pub unusually_large: bool,
    pub mask: Option<ApiMask>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiPage {
    pub id: String,
    pub chapter_id: String,
    pub index: u32,
    /// Stable source slot, used to bind an in-flight hand gesture to this scan.
    pub source_index: usize,
    pub number: u32,
    pub file: String,
    pub source_sha: String,
    pub width: u32,
    pub height: u32,
    pub status: &'static str,
    pub skip_reason: Option<String>,
    /// The page's regions - **only when the page is inside the resident
    /// window**. Empty otherwise, and
    /// `resident` is what tells the two apart.
    pub regions: Vec<ApiRegion>,
    /// How many regions the page has, whether or not they were built. The
    /// interface draws a row count and a review mark from this without asking
    /// for the regions themselves.
    pub region_count: u32,
    /// How many of them are **finished**: masked, and flagged for nothing. The
    /// Pages list's `cleaned / total` ratio and the track it fills.
    ///
    /// A count rather than a derivation, for the same reason `region_count` is
    /// one: outside the resident window there are no regions to derive it from,
    /// and a list that counted `page.regions` drew `0 / 0` for every page but
    /// the three in hand. It is read
    /// out of the manifest, which is what makes it survive a reopen without
    /// anything having to cache it.
    pub done_count: u32,
    /// How many of them need review - `review_reason` over the same regions,
    /// per page, so a row can draw its `✓ 2` mark while the chapter-wide
    /// [`ReviewRef`] index answers the bottom pill.
    pub review_count: u32,
    /// How many of them are held text-group candidates awaiting the user's
    /// choice. Counted in `region_count` (they are listed) and in neither of
    /// the other two: a candidate is not finished work and not a problem.
    pub candidate_count: u32,
    /// Whether `regions` was built for this page.
    ///
    /// A separate field because `regions: []` means two different things - "no
    /// regions" and "not loaded" - and a page list that cannot tell them apart
    /// shows an empty Layers panel for a page full of masks.
    pub resident: bool,
    /// [`crate::tile::page_appearance`]: the cache identity of this page's
    /// `cleaned` tiles, read out of the manifest, so it survives a reload and
    /// a reopen. Present on a header as well as on a resident page.
    pub appearance: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiChapter {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub number: u32,
    pub order: u32,
    pub last_opened: RelativeTime,
    /// The folder this chapter's pages came from, or `""` when the library
    /// cannot say - see [`IndexChapter::source_path`]. A project's own
    /// `sourcePath` is a *different* fact and no longer stands in for this one.
    pub source_path: String,
    pub source_format: String,
    pub no_text_detected: bool,
    pub input_reports: Vec<NoticeSpec>,
    pub pages: Vec<ApiPage>,
    /// Every region in the chapter that needs review, in page order. See
    /// [`ReviewRef`] - this is what makes the review set answerable without the
    /// chapter's regions being resident.
    pub review: Vec<ReviewRef>,
    /// Set when every page has a file from a denoise run still on disk and at
    /// least one can be taken as its page: the chapter menu's "Replace pages
    /// with denoised" ([`crate::page_denoise::replacement`]).
    pub denoise_replacement: Option<crate::page_denoise::DenoiseReplacement>,
    /// Set once the chapter has been denoised: how many runs are remembered,
    /// the newest, and whether one was taken as the pages
    /// ([`crate::denoise_history::summary`]). The chapter row's history menu.
    pub denoise_history: Option<crate::denoise_history::DenoiseSummary>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterruptedJob {
    pub chapter_id: String,
    pub page_index: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiProject {
    pub id: String,
    pub name: String,
    pub mode: StripMode,
    pub reading_direction: ReadingDirection,
    pub created: String,
    pub app_version: &'static str,
    pub source_path: String,
    pub last_opened: RelativeTime,
    pub starred: bool,
    pub interrupted_job: Option<InterruptedJob>,
    /// Always `None` - conversion happens at ingest, so there is never one
    /// pending. See [`Library::open_chapter`].
    pub conversion: Option<serde_json::Value>,
    pub chapters: Vec<ApiChapter>,
}

/// What `createChapter` did.
///
/// Three outcomes rather than `Option<ApiChapter>`, because the seam's `null`
/// used to mean two things at once and one of them is now a thing the user can
/// act on. The command flattens this back to `ApiChapter|null` - that is the
/// shape the seam fixes - and puts the difference
/// where a difference the user can act on belongs, on the notice stack.
#[derive(Debug, Clone)]
pub enum NewChapter {
    Created(Box<ApiChapter>),
    /// No project in the library has that id.
    NoProject,
    /// Nothing said where this chapter's pages are, and the folder that would
    /// have been inferred is already read by the named chapter. Refused rather
    /// than created, because a chapter whose source is wrong cannot be
    /// corrected afterwards: the seam has no `setChapterSource` and no
    /// `deleteChapter`, so a chapter created over the wrong folder is a chapter
    /// the user is stuck with.
    SourceTaken {
        chapter: String,
    },
}

/// What became of a chapter's **source scans** when the chapter was deleted.
///
/// Deleting a chapter always removes the library's own record of it - the index
/// row, the job manifest and the sidecar directory of masks and patches. The
/// scans are the user's own files, and whether they go is the user's answer to
/// a question the dialog asks outright, so the answer has to come back: a
/// notice that said "deleted" over a folder still on disk would be a lie the
/// user has no way to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Scans {
    /// Not asked for. The folder is untouched.
    Kept,
    /// Asked for, and removed.
    Removed,
    /// Asked for, and **refused**: the chapter reads the project's own folder,
    /// which is where every other chapter of that project looks by default.
    /// Deleting one chapter must not empty the project.
    KeptProjectFolder,
    /// Asked for, and there is nothing to remove: the library has no record of
    /// where this chapter's pages came from (see [`IndexChapter::source_path`]).
    KeptUnknown,
}

/// The result of deleting one chapter.
#[derive(Debug, Clone, Copy)]
pub enum ChapterDeleted {
    /// No project or no chapter in the library has that id.
    NoChapter,
    Deleted {
        number: u32,
        scans: Scans,
    },
}

impl NewChapter {
    /// The chapter, where one was created. `None` collapses the two refusals
    /// back together and is for callers that only need to know whether there is
    /// a chapter - the command distinguishes them, because only it can say so.
    pub fn created(self) -> Option<ApiChapter> {
        match self {
            NewChapter::Created(chapter) => Some(*chapter),
            NewChapter::NoProject | NewChapter::SourceTaken { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedChapter {
    pub project: ApiProject,
    pub chapter: ApiChapter,
    /// Always `None`, and not because nobody has got to it - see
    /// [`Library::open_chapter`] for the argument.
    pub pending_conversion: Option<serde_json::Value>,
}

/* ------------------------------------------------------------------ */
/* Time                                                                */
/* ------------------------------------------------------------------ */

/// Epoch seconds as an ISO 8601 instant in UTC.
///
/// The core stores `created` as a
/// number and `src/lib/model/types.js` declares it a string. Written by hand
/// rather than with a date crate because this is the whole of the requirement -
/// one direction, UTC, seconds - and the civil-from-days arithmetic is
/// exhaustively testable in a way a dependency's configuration is not.
pub fn iso8601(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let rest = epoch_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest / 60) % 60,
        rest % 60,
    )
}

/// Days since the epoch to a civil date. Howard Hinnant's `civil_from_days`,
/// which is exact for every day in the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// How long ago, as an i18n key and its parameters.
///
/// The catalogue's buckets are the whole vocabulary - `justNow`, `hoursAgo`,
/// `yesterday`, `daysAgo`, `lastWeek`, `weeksAgo` - so there is no minute
/// bucket and nothing above weeks. A project last opened a year ago reads
/// "52 weeks ago", which is true and clumsy; a month bucket is a catalogue
/// entry, not something this module may invent.
pub fn relative_time(now: u64, then: u64) -> RelativeTime {
    let elapsed = now.saturating_sub(then);
    let count = |n: u64| serde_json::json!({ "count": n });
    let none = || serde_json::json!({});
    match elapsed {
        s if s < 3_600 => RelativeTime {
            key: "time.relative.justNow",
            params: none(),
        },
        s if s < 86_400 => RelativeTime {
            key: "time.relative.hoursAgo",
            params: count(s / 3_600),
        },
        s if s < 172_800 => RelativeTime {
            key: "time.relative.yesterday",
            params: none(),
        },
        s if s < 604_800 => RelativeTime {
            key: "time.relative.daysAgo",
            params: count(s / 86_400),
        },
        s if s < 1_209_600 => RelativeTime {
            key: "time.relative.lastWeek",
            params: none(),
        },
        s => RelativeTime {
            key: "time.relative.weeksAgo",
            params: count(s / 604_800),
        },
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/* ------------------------------------------------------------------ */
/* Geometry                                                            */
/* ------------------------------------------------------------------ */

/// A pixel rectangle as percentages of the page it sits on.
///
/// `ApiRegion.bbox` is in the region's own normalised space - `PAGE_SPAN` is
/// 100 in `src/lib/editor/gesture.js`, and the canvas, the Layers panel and
/// every gesture work in it. The manifest's `bbox` is pixels. A page of zero
/// width has no percentage to give, so it collapses to the origin rather than
/// dividing by it.
pub fn percent_of(rect: Rect, width: u32, height: u32) -> Bbox {
    let span = |value: f64, total: u32| {
        if total == 0 {
            0.0
        } else {
            value * 100.0 / total as f64
        }
    };
    Bbox {
        x: span(rect.x as f64, width),
        y: span(rect.y as f64, height),
        w: span(rect.w as f64, width),
        h: span(rect.h as f64, height),
    }
}

/* ------------------------------------------------------------------ */
/* Regions, out of the manifest                                        */
/* ------------------------------------------------------------------ */

/// What a persisted `review_state` says about the region it belongs to.
///
/// `review_state` is the `review.reason.*` key the region was flagged under,
/// and `src/lib/model/review.js`
/// derives that key from the region's own fields. Restoring the fields from
/// the key is that mapping run backwards, and it is the only way a resumed job
/// shows the same review list it showed before it stopped - the interface
/// re-derives the reason and would find nothing to derive it from.
///
/// **Legacy keys are migrated only where their meaning is known**, here on
/// the load path and never by rewriting the manifest, so an older build
/// still reads what it wrote:
///
/// - `cloudAccepted` was raised on every accepted cloud render, whatever the
///   result looked like. It diagnosed nothing, and a successful cloud layer
///   is not a problem for having used the cloud, so it restores no flag.
/// - The cloud rejection keys name a processing failure and stay flagged.
/// - The grouping keys a cleaned text group can carry (`crossesBalloon`,
///   `maskMissingUnderBox`) are evidence against its mask and stay flagged
///   under their own key; before they had a field they were dropped here,
///   which hid a real reason.
/// - Any other key is one this build has no meaning for. It stays flagged,
///   as `unrecognized`, rather than being cleared: clearing a flag nobody
///   understood is the one migration that cannot be undone by looking.
#[derive(Debug, Default, Clone, PartialEq)]
struct ReviewFlags {
    fitting_reconstructed: bool,
    unusually_large: bool,
    cloud_outcome: Option<(bool, Option<&'static str>)>,
    dependency_review: Option<&'static str>,
    mask_review: Option<&'static str>,
    generated_texture_review: bool,
}

/// The grouping keys a held text-group candidate is stored under
/// (`cleaner_core::text_groups::ReviewReason::key`): lettering no text box
/// claimed, while a box detector ran, or a lone island in SAM-only mode.
/// Nothing else writes them, so on an untouched row they mean "candidate".
pub(crate) fn is_candidate_reason(key: &str) -> bool {
    matches!(key, "review.reason.unassignedMask" | "review.reason.isolatedMask")
}

/// Regions whose cloud work needs a look, by region id, with every attempt
/// that says so and why ([`CLOUD_ATTENTION_KEYS`]).
///
/// **Held here, not in the manifest,** because the fact is the attempt
/// journal's, not the chapter's: an attempt committed a result the manifest
/// no longer shows, or holds a result that was never applied. The journal is
/// read for it at every start whether or not the cloud is allowed
/// (`inference::commands::refresh_cloud_attention`), because a lost result is
/// the same fact with the cloud switched off, and again after a recovery has
/// settled what it could. The region then carries its reason in Layers, in its
/// page's review count and in the review index. Acknowledging or abandoning an
/// attempt takes it off at once.
///
/// Several attempts can name one region (a render and its retry). Each keeps
/// its own entry, so settling one leaves the region flagged for the other.
#[derive(Default)]
struct CloudAttention {
    /// region id -> attempt id -> reason key.
    regions: HashMap<String, std::collections::BTreeMap<String, &'static str>>,
    /// A read of the journal is under way; lookups wait for it rather than
    /// answer from a set it is about to replace.
    reading: bool,
}

/// The reasons a region's cloud work is flagged under, most urgent first.
/// Only `repairNeeded` says a committed result went missing.
pub(crate) const CLOUD_ATTENTION_KEYS: [&str; 3] = [
    "review.reason.repairNeeded",
    "review.reason.cloudResultNotApplied",
    "review.reason.cloudResultUnchecked",
];

fn cloud_attention() -> &'static (Mutex<CloudAttention>, std::sync::Condvar) {
    static ATTENTION: OnceLock<(Mutex<CloudAttention>, std::sync::Condvar)> = OnceLock::new();
    ATTENTION.get_or_init(Default::default)
}

fn attention_lock() -> std::sync::MutexGuard<'static, CloudAttention> {
    cloud_attention().0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One journal read's findings, for [`replace_cloud_attention`].
#[derive(Debug, Default)]
pub(crate) struct CloudAttentionScan {
    /// `(region id, attempt id, reason key)`.
    pub flags: Vec<(String, String, &'static str)>,
    /// Attempts the read could not judge this time (one a render in this
    /// process is working on, or a record mid-write). Whatever they flagged
    /// before stays until a read can judge them.
    pub unjudged: Vec<String>,
}

/// Page lookups wait from here until [`replace_cloud_attention`], so a page
/// read at start-up does not answer from a set that is still being read.
pub(crate) fn begin_cloud_attention_read() {
    attention_lock().reading = true;
}

/// Replace the flagged regions with what a read of the journal found, keeping
/// the entries of attempts it could not judge.
pub(crate) fn replace_cloud_attention(scan: CloudAttentionScan) {
    let mut attention = attention_lock();
    let mut regions: HashMap<String, std::collections::BTreeMap<String, &'static str>> = HashMap::new();
    for (region_id, attempts) in &attention.regions {
        for (attempt_id, key) in attempts {
            if scan.unjudged.contains(attempt_id) {
                regions.entry(region_id.clone()).or_default().insert(attempt_id.clone(), key);
            }
        }
    }
    for (region_id, attempt_id, key) in scan.flags {
        if !region_id.is_empty() {
            regions.entry(region_id).or_default().insert(attempt_id, key);
        }
    }
    attention.regions = regions;
    attention.reading = false;
    cloud_attention().1.notify_all();
}

/// A journal read that could not finish: lookups stop waiting for it, and
/// what was flagged before stays.
pub(crate) fn end_cloud_attention_read() {
    attention_lock().reading = false;
    cloud_attention().1.notify_all();
}

/// The user settled an attempt: no region is flagged for it any more. A
/// region another attempt still flags stays flagged.
pub(crate) fn forget_repair(attempt_id: &str) {
    let mut attention = attention_lock();
    attention.regions.retain(|_, attempts| {
        attempts.remove(attempt_id);
        !attempts.is_empty()
    });
}

/// The most urgent cloud reason a region is flagged under, if any.
fn cloud_attention_of(region_id: &str) -> Option<&'static str> {
    let (_, ready) = cloud_attention();
    let attention = attention_lock();
    // Bounded: a journal read that never finishes must not hang the library,
    // and after one wait runs out no later lookup waits on it again.
    let (mut attention, waited) = ready
        .wait_timeout_while(attention, std::time::Duration::from_secs(30), |attention| attention.reading)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if waited.timed_out() {
        attention.reading = false;
    }
    let attempts = attention.regions.get(region_id)?;
    CLOUD_ATTENTION_KEYS.into_iter().find(|key| attempts.values().any(|found| found == key))
}

/// The grouping keys that are evidence against a mask that was, or was to be,
/// cleaned: the lettering reaches out of its balloon, or a text box had no
/// lettering under it. Returned as the static key the seam carries.
fn mask_evidence_key(key: &str) -> Option<&'static str> {
    match key {
        "review.reason.crossesBalloon" => Some("review.reason.crossesBalloon"),
        "review.reason.maskMissingUnderBox" => Some("review.reason.maskMissingUnderBox"),
        _ => None,
    }
}

fn review_flags(review_state: Option<&str>) -> ReviewFlags {
    let mut flags = ReviewFlags::default();
    let Some(key) = review_state else {
        return flags;
    };
    match key {
        "review.reason.inputChanged" => flags.dependency_review = Some("changed"),
        "review.reason.inputUnknown" => flags.dependency_review = Some("unknown"),
        "review.reason.fittingReconstructed" => flags.fitting_reconstructed = true,
        "review.reason.unusuallyLarge" => flags.unusually_large = true,
        "review.reason.checkGeneratedTexture" => flags.generated_texture_review = true,
        // Legacy, and migrated: see above.
        "review.reason.cloudAccepted" => {}
        "review.reason.cloudRejectedSafetyFilter" => {
            flags.cloud_outcome = Some((false, Some("safety-filter")))
        }
        "review.reason.cloudRejectedTransportError" => {
            flags.cloud_outcome = Some((false, Some("transport-error")))
        }
        "review.reason.cloudRejectedParameterTest" => {
            flags.cloud_outcome = Some((false, Some("parameter-test")))
        }
        "review.reason.cloudRejectedResidualTest" => {
            flags.cloud_outcome = Some((false, Some("residual-test")))
        }
        "review.reason.cloudRejectedStructural" => {
            flags.cloud_outcome = Some((false, Some("structural")))
        }
        // A candidate is only ever cleaned because somebody chose it; what it
        // was held for is answered by that choice.
        candidate if is_candidate_reason(candidate) => {}
        other => {
            flags.mask_review = Some(mask_evidence_key(other).unwrap_or("review.reason.unrecognized"))
        }
    }
    flags
}

/// The fill mode a patch was written with.
///
/// Recorded in `params_snapshot` when the engine put it there - the
/// field is "the thresholds, dilation, radii **actually used**", and the fill mode
/// is one of them - and derived from the rung otherwise: rung 0 measures the
/// paper around the mask, everything above it reconstructs.
fn fill_mode(engine: Engine, params: &serde_json::Value) -> &'static str {
    match params.get("fill_mode").and_then(|value| value.as_str()) {
        Some("match-surround") => return "match-surround",
        Some("reconstruct") => return "reconstruct",
        Some("solid") => return "solid",
        _ => {}
    }
    match engine {
        Engine::Fill => "match-surround",
        Engine::Lama | Engine::Flux | Engine::Cloud => "reconstruct",
        // **Neither word fits a hand tool, and `solid` is the closest of the
        // three.** The seam's `fillMode` is a closed union of `match-surround`,
        // `reconstruct` and `solid` (`src/lib/model/types.js`), and a painted
        // stroke matched nothing and reconstructed nothing - its pixels were
        // *given*, which is what `solid` says about rung 0's flat fill and is
        // the nearest true thing available. The paint branch writes the field
        // into its own snapshot, so this arm only answers for a record written
        // before it did. Recorded as a deferral.
        Engine::Paint | Engine::Clone => "solid",
    }
}

/// A patch whose mask has been deleted, **as the edit that deleted it reports
/// it** - and nowhere else.
///
/// A deleted mask is not a region any more. The record stays on disk with
/// `visible` off, because that is what makes the delete undoable, but nothing
/// that *lists* regions emits it: [`page_of`] skips it, so it is absent from
/// the Layers panel, from the canvas, from the page's three counts and from the
/// review index. Emitting it as a pending, maskless region put an "unexamined /
/// no mask" row and a box on the page exactly where the user had just deleted
/// one, which is the opposite of what deleting means.
///
/// This shape survives for the one caller that still needs it: `deleteMask` and
/// its `restoreRegion` twin answer with the region *they just acted on*, and
/// "no mask, `outcome: pending`" is the true description of what is left.
///
/// A soft delete rather than a hard one, deliberately. `visible` is a persisted
/// field the composite already honours - `tile.rs` and `exporting.rs` both
/// filter on it - so turning it off restores the original text on the cleaned
/// page and in the export in one write, and turning it back on is the whole of
/// undo. Dropping the row and its buffers instead would make the undo half of
/// the seam's own contract impossible to serve.
fn region_of_deleted_patch(record: &PatchRecord, page: &ApiPage) -> ApiRegion {
    ApiRegion {
        id: record.id.clone(),
        page_id: page.id.clone(),
        source_sha: page.source_sha.clone(),
        bbox: percent_of(record.bbox, page.width, page.height),
        source: match record
            .provenance
            .params_snapshot
            .get("source")
            .and_then(|v| v.as_str())
        {
            Some("hand") => "hand",
            _ => "auto",
        },
        detected: true,
        padding_px: None,
        outcome: "pending",
        gate_skip_cause: None,
        decline_reason: None,
        candidate_reason: None,
        attention: None,
        candidate_inside_bubble: None,
        inside_bubble: None,
        unusually_large: false,
        mask: None,
    }
}

pub(crate) fn region_of_patch(record: &PatchRecord, page: &ApiPage) -> ApiRegion {
    if !record.visible {
        return region_of_deleted_patch(record, page);
    }
    let flags = review_flags(record.review_state.as_deref());
    let params = &record.provenance.params_snapshot;
    let layer: cleaner_core::patch::LayerStyle = serde_json::from_value(params.get("layer").cloned().unwrap_or_default())
        .unwrap_or_default();
    let layer = layer.sanitized();
    ApiRegion {
        id: record.id.clone(),
        page_id: page.id.clone(),
        source_sha: page.source_sha.clone(),
        bbox: percent_of(layer.display_bounds(record.bbox), page.width, page.height),
        // A hand mask and an automatic mask are identical in kind;
        // `source` adds one word and changes nothing else, so it
        // rides in the params snapshot rather than in a manifest field.
        source: match params.get("source").and_then(|value| value.as_str()) {
            Some("hand") => "hand",
            _ => "auto",
        },
        detected: true,
        padding_px: None,
        outcome: "cleaned",
        gate_skip_cause: None,
        decline_reason: None,
        candidate_reason: None,
        attention: None,
        candidate_inside_bubble: None,
        inside_bubble: None,
        unusually_large: flags.unusually_large,
        mask: Some(ApiMask {
            id: format!("{}-m1", record.id),
            geometry_policy: (record.geometry_policy == cleaner_core::text_shape::GeometryPolicy::TextShape)
                .then_some(record.geometry_policy),
            text_shape_plan_identity: record.text_shape_plan_identity.clone(),
            text_shape_patch_revision: record.text_shape_revision_id(),
            legacy_patch_revision: record.legacy_revision_id(),
            mask_quality_state: None,
            write_support_sha256: params.get("write_support_sha256")
                .and_then(|value| value.as_str()).map(str::to_owned),
            region_id: record.id.clone(),
            layer,
            capabilities: record.provenance.engine.layer_capabilities(),
            source_bbox: percent_of(record.bbox, page.width, page.height),
            appearance: Some(crate::tile::record_appearance(record)),
            layer_key: Some(crate::tile::layer_appearance(record)),
            order: Some(record.order),
            // The manifest keeps one record per region id - a re-run replaces
            // it rather than appending - so a reopened job has exactly one
            // revision and it is the first one the session sees.
            sequence: 1,
            fill_mode: fill_mode(record.provenance.engine, params),
            elapsed_ms: params
                .get("elapsed_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            fitting_reconstructed: flags.fitting_reconstructed,
            dependency_review: flags.dependency_review,
            mask_review: flags.mask_review,
            generated_texture_review: flags.generated_texture_review,
            cloud_outcome: flags
                .cloud_outcome
                .map(|(accepted, rejection_cause)| CloudOutcome {
                    accepted,
                    rejection_cause,
                }),
            provenance: ApiProvenance::of(&record.provenance),
        }),
    }
}

/// A stored detection, as the seam shows it: `outcome: "detected"`, with the
/// mask it will be cleaned through (`docs/detect-clean.md` §2).
///
/// **The mask is a promise, not an edit.** It has the shape every mask has, so
/// the canvas and the Layers panel draw it with the code they already have, and
/// its provenance says so in the one way the shape allows: the engine is the
/// rung the region will start on, and the snapshot carries `detected: true`
/// with the stored pick, the detector and the balloon answers, which is what a
/// Clean or a cloud clean needs to know about it. There are no pixels, and so
/// no revision for an undo to name and nothing a review would flag: a size
/// warning belongs to the patch that cleans it, and goes there.
pub(crate) fn region_of_detection(record: &DetectedRegion, page: &ApiPage) -> ApiRegion {
    let engine = match record.pick.as_str() {
        "lama" => Engine::Lama,
        _ => Engine::Fill,
    };
    let fill_mode = match record.pick.as_str() {
        "solid" => "solid",
        "lama" => "reconstruct",
        _ => "match-surround",
    };
    let params = serde_json::json!({
        "source": "auto",
        "detected": true,
        "pick": record.pick,
        "detector": record.detector,
        "inside": record.inside,
        "script": record.script,
        "balloon_color": record.balloon_color
            .map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}")),
        "fill_mode": fill_mode,
    });
    ApiRegion {
        id: record.id.clone(),
        page_id: page.id.clone(),
        source_sha: page.source_sha.clone(),
        bbox: percent_of(record.bbox, page.width, page.height),
        source: "auto",
        detected: true,
        padding_px: Some(record.padding_px),
        outcome: "detected",
        gate_skip_cause: None,
        decline_reason: None,
        candidate_reason: None,
        attention: None,
        candidate_inside_bubble: None,
        inside_bubble: Some(record.inside),
        unusually_large: false,
        mask: Some(ApiMask {
            id: format!("{}-m1", record.id),
            geometry_policy: None,
            text_shape_plan_identity: None,
            text_shape_patch_revision: None,
            legacy_patch_revision: None,
            mask_quality_state: None,
            write_support_sha256: None,
            region_id: record.id.clone(),
            // One more than the hand edits its masks have had, so a mask edit
            // moves it even where the mask's digest does not (a remove of
            // lettering that lay outside the mask), and the canvas's mask URL
            // with it (`api/tile.js#detectionMaskUrl`).
            sequence: u32::try_from(record.edits()).unwrap_or(u32::MAX).saturating_add(1),
            fill_mode,
            elapsed_ms: 0,
            fitting_reconstructed: false,
            dependency_review: None,
            mask_review: None,
            generated_texture_review: false,
            cloud_outcome: None,
            provenance: ApiProvenance {
                engine,
                engine_version: env!("CARGO_PKG_VERSION").to_owned(),
                model_sha256: None,
                // Where the detection ran, the one execution fact it has.
                execution_provider: record.detector.clone(),
                params_snapshot: params,
                mask_sha256: record.mask_sha256.clone(),
                source_sha256: page.source_sha.clone(),
                cloud: None,
                created: iso8601(record.created),
            },
            layer: cleaner_core::patch::LayerStyle::default(),
            capabilities: cleaner_core::patch::LayerCapabilities::DETECTION,
            source_bbox: percent_of(record.bbox, page.width, page.height),
            appearance: None,
            layer_key: None,
            order: None,
        }),
    }
}

/// A stored detection's record with its text type set: `inside` a speech
/// balloon or not. Answers whether anything changed; a record that already
/// has the type is left exactly as it was.
///
/// `inside` is what a later Clean starts the region by (the bubble pick or the
/// outside pick, `run.rs`), and it is the only thing the flip is about. With it:
///
/// - **`balloon_color` is cleared.** It is the paper tone measured around the
///   lettering when Detect judged it inside a balloon, and a type the user
///   corrected is not a measurement. A Solid clean then paints the run's own
///   colour, the fallback every detection outside a balloon already takes.
/// - **`pick` changes only where it would contradict the new type.** The
///   stored pick is the rung a Layers row's Clean starts on. `fill` and
///   `solid` paint flat paper, which only a balloon promises, so outside one
///   they become `lama`: the outside default and the rung that needs no
///   balloon, which is also where a hand-made detection starts
///   (`region.rs#hand_detection`). Every rung can clean inside a balloon, so a
///   region moved inside keeps its pick. A Clean with picks of its own (Text
///   cleanup's two rows, a cloud clean's repick) reads the side and not this
///   field, so it follows the new type whatever the pick says.
///
/// Everything else (the masks, the fit, the group, the script, the order)
/// stays as Detect left it.
fn retype_detection(record: &mut DetectedRegion, inside: bool) -> bool {
    if record.inside == inside {
        return false;
    }
    record.inside = inside;
    record.balloon_color = None;
    if !inside && matches!(record.pick.as_str(), "fill" | "solid") {
        record.pick = "lama".into();
    }
    true
}

pub(crate) fn region_of_untouched(record: &RegionUntouched, page: &ApiPage) -> ApiRegion {
    let (outcome, gate_skip_cause, decline_reason) = match record.reason.as_str() {
        "review.reason.gateSkippedLowConfidence" => ("gate-skipped", Some("low-confidence"), None),
        "review.reason.gateSkippedOutsideBubble" => ("gate-skipped", Some("outside-bubble"), None),
        "review.reason.gateSkippedNotJapanese" => ("gate-skipped", Some("not-japanese"), None),
        "review.reason.gateSkippedNotText" => ("gate-skipped", Some("not-text"), None),
        "review.reason.languageSkipped" => ("gate-skipped", Some("language-skipped"), None),
        "review.reason.outsideLanguageUnverified" => ("gate-skipped", Some("outside-language-unverified"), None),
        "review.reason.declined" => ("declined", None, None),
        // A text-group candidate: lettering no text box claimed, held so that
        // artwork is never erased on a guess. Listed for the user to choose,
        // not declined - nothing was tried on it and nothing failed.
        candidate if is_candidate_reason(candidate) => ("candidate", None, None),
        // Anything else - a `decline.reason.*` key, a grouping key held for
        // what it says about the mask (`maskMissingUnderBox`), or a key added
        // after this build - leaves the region flagged rather than dropping it
        // out of review, which is the rule `review.js` states for the same
        // case: an imprecisely named cause is better than a region nobody is
        // told about. [`review_reason`] names the grouping keys precisely.
        other => ("declined", None, Some(other.to_owned())),
    };
    let candidate_reason = (outcome == "candidate").then(|| record.reason.clone());
    ApiRegion {
        // Derived from the geometry rather than from the row's position:
        // `regions_untouched` has no id of its own, and a position would change
        // under any rewrite of the list.
        //
        // The whole rectangle, not its corner. Two detections that begin at the
        // same pixel and run to different sizes are two regions, and an id from
        // `x` and `y` alone made them one - the Layers panel would list one, and
        // `cleanAnyway` would address whichever the lookup found first.
        id: format!(
            "{}-u{}-{}-{}-{}",
            page.id, record.bbox.x, record.bbox.y, record.bbox.w, record.bbox.h
        ),
        page_id: page.id.clone(),
        source_sha: page.source_sha.clone(),
        bbox: percent_of(record.bbox, page.width, page.height),
        source: "auto",
        detected: true,
        padding_px: None,
        outcome,
        gate_skip_cause,
        decline_reason,
        candidate_reason,
        attention: None,
        candidate_inside_bubble: if outcome == "candidate" { record.inside_bubble } else { None },
        inside_bubble: None,
        unusually_large: false,
        mask: None,
    }
}

/* ------------------------------------------------------------------ */
/* The library                                                         */
/* ------------------------------------------------------------------ */

#[derive(Clone)]
struct ManifestCacheEntry {
    mtime: SystemTime,
    len: u64,
    project: Arc<Project>,
}

fn manifest_cache() -> &'static Mutex<HashMap<PathBuf, ManifestCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, ManifestCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn invalidate_manifest_cache(path: &Path) {
    let mut cache = manifest_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.remove(path);
}

#[cfg(test)]
fn parse_counts() -> &'static Mutex<HashMap<PathBuf, usize>> {
    static COUNTS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
    COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
pub fn parse_count(path: &Path) -> usize {
    let counts = parse_counts().lock().unwrap_or_else(|p| p.into_inner());
    counts.get(path).copied().unwrap_or(0)
}

#[cfg(test)]
pub fn reset_parse_count(path: &Path) {
    let mut counts = parse_counts().lock().unwrap_or_else(|p| p.into_inner());
    counts.remove(path);
}

/// The library rooted at a directory.
///
/// Everything is expressed against a plain path rather than a `tauri::AppHandle`
/// so that the model is testable without a Tauri runtime; the commands at the
/// bottom of this file are the only part that knows where the root is.
pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn at(root: impl Into<PathBuf>) -> Library {
        Library { root: root.into() }
    }

    pub fn for_app(app: &tauri::AppHandle) -> Result<Library, LibraryError> {
        use tauri::Manager;
        let dir = app.path().app_data_dir().map_err(|e| LibraryError::Index {
            path: PathBuf::from(LIBRARY_DIR),
            detail: e.to_string(),
        })?;
        Ok(Library::at(dir.join(LIBRARY_DIR)))
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join(INDEX_FILE)
    }

    /// The index's writer lock, held across every read-modify-write of it.
    /// The job lock's two halves ([`crate::run::lock_job`]) on `index.json`,
    /// because the index has the same failure a manifest has: a second
    /// process saving an index it read before a chapter was created drops that
    /// chapter's row and hands its id out again. Taken before any job lock,
    /// never after one.
    fn lock_index(&self) -> Result<crate::run::JobLock, LibraryError> {
        // The lock file lives beside the index, so the root has to be there
        // first; `save` would make it a moment later anyway.
        let _ = std::fs::create_dir_all(&self.root);
        Ok(crate::run::lock_job(&self.index_path())?)
    }

    /// Where a chapter's job lives. Derived from the ids rather than stored, so
    /// the whole library is one relocatable directory.
    pub fn job_path(&self, project_id: &str, chapter_id: &str) -> PathBuf {
        self.root
            .join(project_id)
            .join(format!("{chapter_id}.{}", cleaner_core::project::EXTENSION))
    }

    /// Read a chapter's project manifest from disk, returning a cached [`Project`]
    /// if the file on disk has not changed (mtime and size match).
    pub fn read_manifest(&self, path: &Path) -> Result<Arc<Project>, LibraryError> {
        let metadata = std::fs::metadata(path).map_err(|e| LibraryError::Manifest {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        let mtime = metadata.modified().map_err(|e| LibraryError::Manifest {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        let len = metadata.len();

        {
            let cache = manifest_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = cache.get(path) {
                if entry.mtime == mtime && entry.len == len {
                    return Ok(entry.project.clone());
                }
            }
        }

        #[cfg(test)]
        {
            let mut counts = parse_counts().lock().unwrap_or_else(|p| p.into_inner());
            *counts.entry(path.to_path_buf()).or_insert(0) += 1;
        }

        let job = Job::open(path).map_err(|e| LibraryError::Manifest {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        let project = Arc::new(job.project);

        {
            let mut cache = manifest_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.insert(
                path.to_path_buf(),
                ManifestCacheEntry {
                    mtime,
                    len,
                    project: project.clone(),
                },
            );
        }

        Ok(project)
    }

    /// Read the index. A library that has never been written is an empty one,
    /// not an error - that is a first launch.
    ///
    /// An index this build does not know the version of is **refused**, the way
    /// [`Job::open`] refuses a later manifest. `version` was written and never
    /// read, which is worse than not having the field: a v2 index parses as v1
    /// because serde drops what it does not recognise, and the next [`save`]
    /// writes the file back without it. Whatever v2 added is gone, silently and
    /// completely, from a library the user has not touched. The comparison is
    /// `!=` rather than `>` for the same reason `Job::open`'s is: this build can
    /// state what it reads, and cannot state what some other number means.
    ///
    /// [`save`]: Library::save
    pub fn index(&self) -> Result<Index, LibraryError> {
        let path = self.index_path();
        let bytes = match std::fs::read(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Index::default()),
            Err(err) => {
                return Err(LibraryError::Index {
                    path,
                    detail: err.to_string(),
                })
            }
            Ok(bytes) => bytes,
        };
        let mut index: Index = serde_json::from_slice(&bytes).map_err(|e| LibraryError::Index {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        index.seen = Some(ingest::sha256_hex(&bytes));
        if index.version != INDEX_VERSION {
            return Err(LibraryError::Index {
                path,
                detail: format!(
                    "this build reads library index version {INDEX_VERSION}; the file is version {}",
                    index.version
                ),
            });
        }
        Ok(index)
    }

    /// Rewrite the index atomically, for the reason the manifest is rewritten
    /// atomically: a truncated index is every project gone.
    ///
    /// Only over the file `index` was read from. The index lock keeps every
    /// writer that takes it in turn; one that does not (an older build) and
    /// saved in between would otherwise lose its chapter's row here and see
    /// its id handed out again. So the file is looked at before the write and
    /// again right before the rename, as [`Job::flush`] does, and a moved one
    /// is refused: nothing changes, and the next command reads it afresh.
    fn save(&self, index: &mut Index) -> Result<(), LibraryError> {
        let path = self.index_path();
        let refuse = |detail: String| LibraryError::Index { path: path.clone(), detail };
        let unmoved = || -> Result<(), LibraryError> {
            let on_disk = match std::fs::read(&path) {
                Ok(bytes) => Some(ingest::sha256_hex(&bytes)),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => return Err(refuse(err.to_string())),
            };
            if on_disk != index.seen {
                return Err(refuse("the index changed on disk after it was read; the change was refused".into()));
            }
            Ok(())
        };
        unmoved()?;
        std::fs::create_dir_all(&self.root).map_err(|e| LibraryError::Index {
            path: self.root.clone(),
            detail: e.to_string(),
        })?;
        let bytes = serde_json::to_vec_pretty(&*index).map_err(|e| refuse(e.to_string()))?;
        let staged = buffers::stage(&path, &bytes).map_err(|e| refuse(e.to_string()))?;
        unmoved()?;
        staged.publish().map_err(|e| refuse(e.to_string()))?;
        index.seen = Some(ingest::sha256_hex(&bytes));
        Ok(())
    }

    /* ---------- reads ---------- */

    pub fn list_projects(&self) -> Result<Vec<ApiProject>, LibraryError> {
        let index = self.index()?;
        let at = now();
        Ok(index
            .projects
            .iter()
            .map(|project| self.build_project(project, at))
            .collect())
    }

    /// Resolve a chapter id to its `.mtclean` job path.
    ///
    /// The contract two other command modules are written against. It fails
    /// rather than returning a path that is not there, so a caller gets
    /// "this chapter has no job yet" instead of a file-not-found from three
    /// layers down.
    pub fn resolve_chapter(&self, chapter_id: &str) -> Result<PathBuf, LibraryError> {
        let index = self.index()?;
        for project in &index.projects {
            if project
                .chapters
                .iter()
                .any(|chapter| chapter.id == chapter_id)
            {
                let path = self.job_path(&project.id, chapter_id);
                return if path.exists() {
                    Ok(path)
                } else {
                    Err(LibraryError::NoJob {
                        chapter_id: chapter_id.to_owned(),
                    })
                };
            }
        }
        Err(LibraryError::Unknown {
            id: chapter_id.to_owned(),
        })
    }

    /// The source index a page index addresses.
    ///
    /// `ApiPage.index` is a position in `strip.order`, and everything the core
    /// takes - `Job::source_path`, `PatchRecord::source_idx` - is an index into
    /// `sources`. They are equal only while nothing has reordered the pages,
    /// and a caller that treats them as interchangeable serves the wrong page
    /// the first time one does.
    ///
    /// This is the one place the rule lives. `tile::render` goes through it and
    /// `exporting` iterates `strip.order` directly, which is the same rule
    /// spelled the other way round; the run's own callers - `runClean`,
    /// `applyTool`, `createRegion` - arrive later and belong here too.
    pub fn resolve_page(project: &Project, page_index: usize) -> Option<usize> {
        project.strip.order.get(page_index).copied()
    }

    /* ---------- mutations ---------- */

    pub fn create_project(
        &self,
        name: &str,
        mode: StripMode,
        source_path: Option<PathBuf>,
        reading_direction: Option<ReadingDirection>,
    ) -> Result<ApiProject, LibraryError> {
        let _lock = self.lock_index()?;
        let mut index = self.index()?;
        let at = now();
        let id = format!("p{}", take_id(&mut index));
        // No first chapter is created. The mock makes one because its chapters
        // are generated; a real chapter's pages come from the project's source
        // folder, and Home already has an empty state that says so.
        index.projects.insert(
            0,
            IndexProject {
                id: id.clone(),
                name: name.to_owned(),
                mode,
                source_path,
                reading_direction: reading_direction.unwrap_or_default(),
                created: at,
                last_opened: at,
                starred: false,
                chapters: Vec::new(),
                cloud_consent: None,
            },
        );
        self.save(&mut index)?;
        let project = index
            .projects
            .iter()
            .find(|p| p.id == id)
            .expect("just inserted");
        Ok(self.build_project(project, at))
    }

    /// Add a chapter to a project, reading its pages from `source_path`.
    ///
    /// **The path is the argument asked for.** A path given is used exactly
    /// as given and nothing is inferred from it; the project's own folder is
    /// a fact about the project and is no longer allowed to become a
    /// chapter's answer behind the user's back.
    ///
    /// A path *not* given still falls back to the inference this module has
    /// always made - a subfolder named after the chapter, else the project's own
    /// folder - because that inference is right for the first chapter of a
    /// freshly pointed-at scan folder and asking for a path that is already on
    /// screen would be ceremony. What has changed:
    /// [`Library::inferred_source_dir`] refuses to hand a folder to a second
    /// chapter, so the fallback can no longer make two chapters that hold the
    /// same files.
    pub fn create_chapter(
        &self,
        project_id: &str,
        name: &str,
        number: Option<u32>,
        source_path: Option<PathBuf>,
    ) -> Result<NewChapter, LibraryError> {
        let _lock = self.lock_index()?;
        let mut index = self.index()?;
        let at = now();
        let chapter_id = format!("c{}", take_id(&mut index));
        let Some(position) = index.projects.iter().position(|p| p.id == project_id) else {
            return Ok(NewChapter::NoProject);
        };

        let (number, mode, source_dir) = {
            let project = &index.projects[position];
            // The number the user chose, stored verbatim. `max + 1` is the
            // fallback for a caller that names none, and is what the dialog
            // starts its field at.
            let number = number.filter(|given| *given >= 1).unwrap_or_else(|| {
                project.chapters.iter().map(|c| c.number).max().unwrap_or(0) + 1
            });
            let resolved = match source_path {
                Some(given) => Some(given),
                None => match self.inferred_source_dir(project, name) {
                    Ok(dir) => dir,
                    Err(chapter) => return Ok(NewChapter::SourceTaken { chapter }),
                },
            };
            (number, project.mode, resolved)
        };

        // The job is written before the index names it: a crash between the
        // two leaves an unreferenced file, which a sweep collects, rather than
        // an index pointing at nothing.
        if let Some(dir) = &source_dir {
            let manifest = self.job_path(project_id, &chapter_id);
            self.write_job(&manifest, dir, mode)?;
        }

        index.projects[position].chapters.insert(
            0,
            IndexChapter {
                id: chapter_id.clone(),
                name: name.to_owned(),
                number,
                source_path: source_dir,
                created: at,
                last_opened: at,
            },
        );
        self.save(&mut index)?;

        let project = &index.projects[position];
        Ok(project
            .chapters
            .iter()
            .enumerate()
            .find(|(_, chapter)| chapter.id == chapter_id)
            .map(|(order, chapter)| {
                // A chapter that has just been ingested has no patch on any
                // page, so a resident window would build nothing; the editor
                // asks for one when it opens the chapter.
                NewChapter::Created(Box::new(
                    self.build_chapter(project, chapter, order as u32, at, &[])
                        .0,
                ))
            })
            .unwrap_or(NewChapter::NoProject))
    }

    /// The folder a chapter reads when `createChapter` was not told one.
    ///
    /// The inference is unchanged - a subfolder named after the chapter wins,
    /// because that is how a scan folder with several chapters in it is actually
    /// laid out; otherwise the project's own folder is the chapter - and the
    /// guard is the whole of the fix. A folder another chapter of the
    /// same project already reads is refused by name, so the failure -
    /// *"two chapters that both fall back ingest the same files"* -
    /// becomes a sentence on the notice stack instead of two chapters that
    /// quietly hold one chapter's pages.
    ///
    /// `Err` carries the name of the chapter that already has it, so the notice
    /// can say which one.
    fn inferred_source_dir(
        &self,
        project: &IndexProject,
        name: &str,
    ) -> Result<Option<PathBuf>, String> {
        let Some(root) = project.source_path.as_deref() else {
            return Ok(None);
        };
        let named = root.join(name);
        let candidate = if named.is_dir() {
            named
        } else if root.is_dir() {
            root.to_path_buf()
        } else {
            return Ok(None);
        };
        for chapter in &project.chapters {
            let taken = self.chapter_source(project, chapter);
            if taken
                .as_deref()
                .is_some_and(|dir| same_dir(dir, &candidate))
            {
                return Err(chapter.name.clone());
            }
        }
        Ok(Some(candidate))
    }

    /// Where a chapter's pages came from, as far as the library can say.
    ///
    /// The recorded answer when there is one; otherwise the folder the
    /// manifest's own sources sit in, which is a *reading* of what was ingested
    /// rather than a re-run of the inference that chose it. The distinction is
    /// the whole reason a chapter created under the old rule needs no migration:
    /// this can be wrong about nothing, because it asks the file that decided.
    fn chapter_source(&self, project: &IndexProject, chapter: &IndexChapter) -> Option<PathBuf> {
        if let Some(recorded) = &chapter.source_path {
            return Some(recorded.clone());
        }
        let manifest_path = self.job_path(&project.id, &chapter.id);
        let manifest = self.read_manifest(&manifest_path).ok()?;
        ingested_from_manifest(&manifest_path, &manifest)
    }

    /// Open a chapter, and record that it was opened.
    ///
    /// `pendingConversion` is always `None`: native PNG, TIFF and JPEG sources
    /// were copied verbatim at ingest; WebP, GIF and BMP were already converted
    /// to PNG in the chapter's page directory and listed in its input report.
    ///
    /// A file in a format even the converting ingest cannot read - AVIF, HEIC,
    /// JPEG XL, all of which need a C decoder this build does not ship - is
    /// still refused as `input.skipReason.notAnImage` and listed with the other
    /// refusals. Offering the dialog for those would put a Convert button on
    /// screen that cannot convert.
    ///
    /// So `convert` is accepted and ignored, and it is kept on the signature
    /// because it is the seam's and the seam is the fixed target.
    pub fn open_chapter(
        &self,
        project_id: &str,
        chapter_id: &str,
        _convert: bool,
    ) -> Result<Option<OpenedChapter>, LibraryError> {
        let _lock = self.lock_index()?;
        let mut index = self.index()?;
        let at = now();
        let Some(position) = index.projects.iter().position(|p| p.id == project_id) else {
            return Ok(None);
        };
        let Some(chapter_position) = index.projects[position]
            .chapters
            .iter()
            .position(|c| c.id == chapter_id)
        else {
            return Ok(None);
        };

        index.projects[position].last_opened = at;
        index.projects[position].chapters[chapter_position].last_opened = at;
        self.save(&mut index)?;

        let project = &index.projects[position];
        let chapter = &project.chapters[chapter_position];
        // Opening a chapter is not a licence to hold it. The window the editor
        // actually wants depends on the position autosave is about to restore,
        // which is the interface's to know, so the chapter arrives as headers
        // plus its review index and `loadPages` fills the window in one further
        // call.
        Ok(Some(OpenedChapter {
            chapter: self
                .build_chapter(project, chapter, chapter_position as u32, at, &[])
                .0,
            project: self.build_project(project, at),
            pending_conversion: None,
        }))
    }

    /// The regions of a window of pages - the other half of the resident window.
    ///
    /// Answers whole [`ApiPage`]s rather than bare region lists so that the
    /// status a page's regions imply travels with them: a page whose last mask
    /// was deleted while it sat outside the window comes back with the status
    /// the manifest now gives it, not the one the header carried when it left.
    pub fn load_pages(
        &self,
        chapter_id: &str,
        indices: &[usize],
    ) -> Result<Vec<ApiPage>, LibraryError> {
        let path = self.resolve_chapter(chapter_id)?;
        let project = self.read_manifest(&path)?;
        let appearances = crate::tile::page_appearances(&project);
        Ok(indices
            .iter()
            .filter_map(|index| {
                let appearance = appearances.get(*index).cloned().unwrap_or_default();
                page_showing(chapter_id, &project, *index, appearance)
            })
            .collect())
    }

    /// Rename the library's label for a project. The folder on disk keeps its
    /// own name - the rename dialog says so.
    pub fn rename_project(
        &self,
        project_id: &str,
        name: &str,
    ) -> Result<Option<ApiProject>, LibraryError> {
        let _lock = self.lock_index()?;
        let mut index = self.index()?;
        let Some(position) = index.projects.iter().position(|p| p.id == project_id) else {
            return Ok(None);
        };
        index.projects[position].name = name.to_owned();
        self.save(&mut index)?;
        Ok(Some(self.build_project(&index.projects[position], now())))
    }

    /// Whether the project holding `chapter_id` has a standing cloud consent
    /// for this endpoint, given with both statements ticked. Anything
    /// unreadable answers `false`: the question is asked.
    pub fn cloud_consent_covers(
        &self,
        chapter_id: &str,
        provider: CloudProvider,
        profile_id: &str,
        origin_fingerprint: &str,
    ) -> bool {
        self.index().ok().is_some_and(|index| {
            index
                .projects
                .iter()
                .find(|p| p.chapters.iter().any(|c| c.id == chapter_id))
                .and_then(|p| p.cloud_consent.as_ref())
                .is_some_and(|consent| {
                    consent.statements_accepted
                        && consent.covers(provider, profile_id, origin_fingerprint)
                })
        })
    }

    /// Set or clear the standing cloud consent of the project holding
    /// `chapter_id`. `false` when no project holds it. An unchanged consent
    /// is not written again, so the grant after every skipped question costs
    /// no index write.
    pub fn set_cloud_consent(
        &self,
        chapter_id: &str,
        consent: Option<CloudConsent>,
    ) -> Result<bool, LibraryError> {
        let _lock = self.lock_index()?;
        let mut index = self.index()?;
        let Some(project) = index
            .projects
            .iter_mut()
            .find(|p| p.chapters.iter().any(|c| c.id == chapter_id))
        else {
            return Ok(false);
        };
        let unchanged = match (&project.cloud_consent, &consent) {
            (Some(old), Some(new)) => {
                old.statements_accepted == new.statements_accepted
                    && old.covers(new.provider, &new.profile_id, &new.origin_fingerprint)
            }
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            project.cloud_consent = consent;
            self.save(&mut index)?;
        }
        Ok(true)
    }

    /// Record the first cloud consent given in a chapter's project. Only an
    /// answer that ticked both statements is recorded: a consent that did
    /// not see them cannot stand for them. Best effort: a failed write means
    /// the next request asks again.
    pub fn record_cloud_consent(
        &self,
        chapter_id: &str,
        provider: CloudProvider,
        profile_id: &str,
        origin_fingerprint: &str,
        statements_accepted: bool,
    ) {
        if !statements_accepted {
            return;
        }
        let consent = CloudConsent {
            provider,
            profile_id: profile_id.to_owned(),
            origin_fingerprint: origin_fingerprint.to_owned(),
            granted_at: now(),
            statements_accepted,
        };
        let _ = self.set_cloud_consent(chapter_id, Some(consent));
    }

    /// Withdraw standing cloud consents: those given for `endpoint` (provider
    /// and profile) when it is named, every project's when it is not. Called
    /// when that endpoint is removed and when cloud engines are turned off,
    /// so turning either back on asks again. Answers how many were cleared;
    /// nothing is written when none were.
    pub fn clear_cloud_consents(
        &self,
        endpoint: Option<(CloudProvider, &str)>,
    ) -> Result<usize, LibraryError> {
        let _lock = self.lock_index()?;
        let mut index = self.index()?;
        let mut cleared = 0;
        for project in &mut index.projects {
            let matches = project.cloud_consent.as_ref().is_some_and(|consent| {
                endpoint.is_none_or(|(provider, profile_id)| {
                    consent.provider == provider && consent.profile_id == profile_id
                })
            });
            if matches {
                project.cloud_consent = None;
                cleared += 1;
            }
        }
        if cleared > 0 {
            self.save(&mut index)?;
        }
        Ok(cleared)
    }

    /// Drop a project from the library.
    ///
    /// The index entry goes and **nothing on disk is deleted** - not the user's
    /// scans, which is what the copy promises, and not the job directory
    /// either. Removing a project is a one-click action on a card; deleting a
    /// chapter's completed regions with it would make it an unrecoverable one.
    /// The orphaned job directory is what the launch sweep is for, and it
    /// has a rule to recognise it by: a directory under the library root that
    /// the index does not name.
    pub fn delete_project(&self, project_id: &str) -> Result<bool, LibraryError> {
        let _lock = self.lock_index()?;
        let mut index = self.index()?;
        let before = index.projects.len();
        if let Some(project) = index.projects.iter().find(|p| p.id == project_id) {
            for chapter in &project.chapters {
                let path = self.job_path(&project.id, &chapter.id);
                invalidate_manifest_cache(&path);
            }
        }
        index.projects.retain(|project| project.id != project_id);
        if index.projects.len() == before {
            return Ok(false);
        }
        self.save(&mut index)?;
        Ok(true)
    }

    /// Delete one chapter.
    ///
    /// **The index row goes first.** The reverse of the order `create_chapter`
    /// uses, and for the same reason: a crash between the two steps must leave
    /// a file the launch sweep can collect rather than an index row pointing at
    /// a manifest that is no longer there.
    ///
    /// What is always removed is the library's own: the row, the job manifest,
    /// and the sidecar directory holding the chapter's masks and patches. The
    /// scans are the user's, so they go only when `source_files` says so - and
    /// not even then when the folder is the project's own, which is where the
    /// project's other chapters look by default. `Scans` carries which of those
    /// happened, because the notice has to say it.
    pub fn delete_chapter(
        &self,
        project_id: &str,
        chapter_id: &str,
        source_files: bool,
    ) -> Result<ChapterDeleted, LibraryError> {
        let _lock = self.lock_index()?;
        let mut index = self.index()?;
        let Some(position) = index.projects.iter().position(|p| p.id == project_id) else {
            return Ok(ChapterDeleted::NoChapter);
        };
        let project = &index.projects[position];
        let Some(at) = project.chapters.iter().position(|c| c.id == chapter_id) else {
            return Ok(ChapterDeleted::NoChapter);
        };

        let chapter = &project.chapters[at];
        let number = chapter.number;
        let source_dir = chapter.source_path.clone();
        let project_root = project.source_path.clone();

        // The chapter's own lock, after the index's (the one order the two are
        // ever taken in) and before anything goes. A writer part way through
        // the chapter finishes first; one that came after would find no
        // manifest and write nothing. Without it a writer between its checks
        // and its buffers put the sidecar back after the delete. Refused, when
        // another process is using the chapter, before the row is touched.
        let manifest = self.job_path(project_id, chapter_id);
        let _job = crate::run::lock_job(&manifest)?;

        index.projects[position].chapters.remove(at);
        self.save(&mut index)?;

        invalidate_manifest_cache(&manifest);
        // Best effort, and deliberately not an error: the row has gone, and a
        // manifest that was already missing is the state this was aiming at.
        // What is left behind is exactly what the launch sweep collects. The
        // lock file goes as `_job` does, now that its manifest has.
        let _ = std::fs::remove_file(&manifest);
        let _ = std::fs::remove_dir_all(cleaner_core::project::sidecar_dir(&manifest));

        let scans = match (source_files, source_dir) {
            (false, _) => Scans::Kept,
            (true, None) => Scans::KeptUnknown,
            (true, Some(dir)) => {
                if project_root
                    .as_ref()
                    .is_some_and(|root| same_dir(root, &dir))
                {
                    Scans::KeptProjectFolder
                } else {
                    std::fs::remove_dir_all(&dir).map_err(|e| LibraryError::Index {
                        path: dir.clone(),
                        detail: e.to_string(),
                    })?;
                    Scans::Removed
                }
            }
        };

        Ok(ChapterDeleted::Deleted { number, scans })
    }

    /* ---------- region-level edits ---------- */

    /// Persist presentation settings without altering the immutable render
    /// artifacts.
    ///
    /// **The capabilities are enforced here, not only in the interface.**
    /// [`LayerEdit::Edit`] is the `set_layer_style` command: a move or
    /// rotation of a fixed redraw, a move of a locked layer, a lock on a
    /// layer with no position, or any style on a detection is refused whole,
    /// with the refusal's catalogue key as the error. [`LayerEdit::Restore`]
    /// is `restoreRegion` replaying history: it puts back what the
    /// capabilities allow and keeps the rest as stored, so an undo cannot move
    /// a fixed redraw either, and cannot stall on a history older than the
    /// rule.
    pub fn set_layer_style(
        &self,
        region_id: &str,
        style: cleaner_core::patch::LayerStyle,
        mode: LayerEdit,
    ) -> Result<Option<(ApiRegion, String)>, LibraryError> {
        let index = self.index()?;
        let Some(chapter_id) = chapter_holding(&index, region_id) else { return Ok(None) };
        let path = self.resolve_chapter(&chapter_id)?;
        let _lock = crate::run::lock_job(&path)?;
        let mut job = Job::open(&path).map_err(|e| LibraryError::Manifest {
            path: path.clone(), detail: e.to_string(),
        })?;
        let Some(record) = job.project.patches.iter().find(|r| r.id == region_id && r.visible) else {
            // A detection has a mask and no output: nothing to fade or place.
            let detection = job.project.detections.iter().any(|record| record.id == region_id);
            if detection && mode == LayerEdit::Edit {
                cleaner_core::patch::LayerStyle::default()
                    .edited(style, cleaner_core::patch::LayerCapabilities::DETECTION)
                    .map_err(LibraryError::LayerRefused)?;
            }
            return Ok(None);
        };
        let source_idx = record.source_idx;
        let current = cleaner_core::project::orientation::display_layer_style(&job.project,record);
        // Where the layer is drawn before this edit: a move takes its pixels
        // away from there, which a later layer that read them has to hear.
        let drawn_before = cleaner_core::project::orientation::display_bbox(&job.project,record);
        let capabilities = record.provenance.engine.layer_capabilities();
        let style = match mode {
            LayerEdit::Edit => current.edited(style, capabilities).map_err(LibraryError::LayerRefused)?,
            LayerEdit::Restore => current.restored(style, capabilities),
        };
        let stored = record.provenance.params_snapshot.get("layer").is_some();
        if style == current && (stored || style == cleaner_core::patch::LayerStyle::default()) {
            // Already so. Answered rather than written: a coalesced preview
            // and an undo that lands where it started both ask this, and a
            // write would flag every later layer for review over nothing.
            return Ok(region_and_status(&chapter_id, &job.project, source_idx, region_id));
        }
        let style=cleaner_core::project::orientation::stored_layer_style(&job.project,record,style);
        let record=job.project.patches.iter_mut().find(|r|r.id==region_id).unwrap();
        if !record.provenance.params_snapshot.is_object() {
            record.provenance.params_snapshot = serde_json::json!({});
        }
        record.provenance.params_snapshot["layer"] = serde_json::to_value(style).unwrap_or_default();
        job.flush().map_err(|e| LibraryError::Manifest {
            path: path.clone(), detail: e.to_string(),
        })?;
        crate::underlay::refresh_dependencies_from(&mut job, region_id, Some(drawn_before))
            .map_err(|detail| LibraryError::Manifest { path: path.clone(), detail })?;
        invalidate_manifest_cache(&path);
        Ok(region_and_status(&chapter_id, &job.project, source_idx, region_id))
    }

    /// Turn one region's mask off or on, and answer with the region as the
    /// manifest now holds it - the seam's `deleteMask` and the restoring half
    /// of its `restoreRegion`.
    ///
    /// Both directions are this one method because they are one write with a
    /// different boolean, and because the pair has to agree about everything
    /// else it touches: which chapter, which page, what the page's status
    /// becomes. Two methods that computed the page status separately is how a
    /// deleted-then-undone mask ends up under a mark that never moved back.
    ///
    /// `None` when no chapter in the library names that region, which is the
    /// same answer the seam gives for an id it cannot find.
    pub fn set_mask_visible(
        &self,
        region_id: &str,
        visible: bool,
    ) -> Result<Option<(ApiRegion, String)>, LibraryError> {
        let index = self.index()?;
        let Some(chapter_id) = chapter_holding(&index, region_id) else {
            return Ok(None);
        };
        let path = self.resolve_chapter(&chapter_id)?;

        // Under the job's own lock, like every other write to a manifest in
        // this crate: a run flushes the whole file after every region, and a
        // read-modify-write that interleaves with one loses it entirely.
        let _lock = crate::run::lock_job(&path)?;
        let mut job = Job::open(&path).map_err(|e| LibraryError::Manifest {
            path: path.clone(),
            detail: e.to_string(),
        })?;

        let Some(record) = job.project.patches.iter_mut().find(|r| r.id == region_id) else {
            return Ok(None);
        };
        if record.visible == visible {
            // Already there. Still answered, not refused: the seam's undo is
            // free to ask twice, and "it is as you asked" is the truth.
            let source_idx = record.source_idx;
            return Ok(region_and_status(
                &chapter_id,
                &job.project,
                source_idx,
                region_id,
            ));
        }
        record.visible = visible;
        let source_idx = record.source_idx;
        job.flush().map_err(|e| LibraryError::Manifest {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        crate::underlay::refresh_dependencies(&mut job, region_id).map_err(|detail| {
            LibraryError::Manifest {
                path: path.clone(),
                detail,
            }
        })?;
        invalidate_manifest_cache(&path);
        Ok(region_and_status(
            &chapter_id,
            &job.project,
            source_idx,
            region_id,
        ))
    }

    /// Restore the exact committed text-shaped revision named by an undo
    /// snapshot. Its immutable patch and plan artifacts are checked by the
    /// store before the current row changes. Never turn on the latest patch
    /// when the requested historical revision has gone missing.
    pub fn restore_text_shape_mask(
        &self,
        region_id: &str,
        revision: &str,
    ) -> Result<Option<(ApiRegion, String)>, LibraryError> {
        let index = self.index()?;
        let Some(chapter_id) = chapter_holding(&index, region_id) else { return Ok(None) };
        let path = self.resolve_chapter(&chapter_id)?;
        let _lock = crate::run::lock_job(&path)?;
        let mut job = Job::open(&path).map_err(|e| LibraryError::Manifest {
            path: path.clone(), detail: e.to_string(),
        })?;
        if !job.restore_text_shape_revision(region_id, revision).map_err(|e| LibraryError::Manifest {
            path: path.clone(), detail: e.to_string(),
        })? {
            return Ok(None);
        }
        let Some(record) = job.project.patches.iter_mut().find(|record| record.id == region_id) else {
            return Ok(None);
        };
        record.visible = true;
        let source_idx = record.source_idx;
        job.flush().map_err(|e| LibraryError::Manifest { path: path.clone(), detail: e.to_string() })?;
        crate::underlay::refresh_dependencies(&mut job, region_id).map_err(|detail| LibraryError::Manifest {
            path: path.clone(), detail,
        })?;
        invalidate_manifest_cache(&path);
        Ok(region_and_status(&chapter_id, &job.project, source_idx, region_id))
    }

    pub fn restore_legacy_mask(
        &self,
        region_id: &str,
        revision: &str,
    ) -> Result<Option<(ApiRegion, String)>, LibraryError> {
        let index = self.index()?;
        let Some(chapter_id) = chapter_holding(&index, region_id) else { return Ok(None) };
        let path = self.resolve_chapter(&chapter_id)?;
        let _lock = crate::run::lock_job(&path)?;
        let mut job = Job::open(&path).map_err(|error| LibraryError::Manifest {
            path: path.clone(), detail: error.to_string(),
        })?;
        if !job.restore_legacy_revision(region_id, revision).map_err(|error| LibraryError::Manifest {
            path: path.clone(), detail: error.to_string(),
        })? {
            return Ok(None);
        }
        let Some(record) = job.project.patches.iter_mut().find(|row| row.id == region_id) else {
            return Ok(None);
        };
        record.visible = true;
        let source_idx = record.source_idx;
        job.flush().map_err(|error| LibraryError::Manifest {
            path: path.clone(), detail: error.to_string(),
        })?;
        crate::underlay::refresh_dependencies(&mut job, region_id).map_err(|detail| LibraryError::Manifest {
            path: path.clone(), detail,
        })?;
        invalidate_manifest_cache(&path);
        Ok(region_and_status(&chapter_id, &job.project, source_idx, region_id))
    }

    pub fn keep_dependency_result(
        &self,
        region_id: &str,
    ) -> Result<Option<ApiRegion>, LibraryError> {
        let index = self.index()?;
        let Some(chapter_id) = chapter_holding(&index, region_id) else {
            return Ok(None);
        };
        let path = self.resolve_chapter(&chapter_id)?;
        let _lock = crate::run::lock_job(&path)?;
        let mut job = Job::open(&path).map_err(|e| LibraryError::Manifest {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        let Some(record) = job
            .project
            .patches
            .iter_mut()
            .find(|r| r.id == region_id && r.visible)
        else {
            return Ok(None);
        };
        if !matches!(
            record.review_state.as_deref(),
            Some("review.reason.inputChanged" | "review.reason.inputUnknown")
        ) {
            return Ok(None);
        }
        let source_idx = record.source_idx;
        record.review_state = record
            .provenance
            .params_snapshot
            .get("review_before_input_change")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        if let Some(snapshot) = record.provenance.params_snapshot.as_object_mut() {
            snapshot.remove("review_before_input_change");
        }
        job.flush().map_err(|e| LibraryError::Manifest {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        invalidate_manifest_cache(&path);
        Ok(
            region_and_status(&chapter_id, &job.project, source_idx, region_id)
                .map(|(region, _)| region),
        )
    }

    /// Delete a stored detection: its record and its two mask files go, and the
    /// page answers with its new status. `None` for an id no detection
    /// carries, which leaves every other kind of region to the call that owns
    /// it.
    pub fn remove_detection(
        &self,
        region_id: &str,
    ) -> Result<Option<(ApiRegion, String)>, LibraryError> {
        let index = self.index()?;
        let Some(chapter_id) = chapter_holding(&index, region_id) else {
            return Ok(None);
        };
        let path = self.resolve_chapter(&chapter_id)?;
        let _lock = crate::run::lock_job(&path)?;
        let mut job = Job::open(&path).map_err(|e| LibraryError::Manifest {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        let Some(record) = job.remove_detection(region_id).map_err(|e| LibraryError::Manifest {
            path: path.clone(),
            detail: e.to_string(),
        })?
        else {
            return Ok(None);
        };
        invalidate_manifest_cache(&path);
        let Some(page_index) = job.project.strip.order.iter().position(|index| *index == record.source_idx)
        else {
            return Ok(None);
        };
        let Some(page) = page_of(&chapter_id, &job.project, page_index) else {
            return Ok(None);
        };
        let status = page.status.to_owned();
        Ok(Some((region_of_detection(&record, &page), status)))
    }

    /// Set a stored detection's text type, inside a speech balloon or outside
    /// one: the region menu's Text type (`setDetectionType`). What changes with
    /// it is [`retype_detection`]'s. The page's status does not move, since the
    /// region is still a detection, and the answer is the region as the
    /// manifest now holds it.
    ///
    /// The same lock and the same manifest write as every other edit of a
    /// detection, so a run holding the chapter refuses it (`job_busy`) and a
    /// manifest that moved under this job refuses the save (`job_stale`).
    /// Nothing is written when the region already has the type asked for.
    ///
    /// `None` for an id no stored detection carries: a cleaned layer, a held
    /// row, a region gone since the menu opened.
    pub fn set_detection_inside(
        &self,
        region_id: &str,
        inside: bool,
    ) -> Result<Option<ApiRegion>, LibraryError> {
        let index = self.index()?;
        let Some(chapter_id) = chapter_holding(&index, region_id) else {
            return Ok(None);
        };
        let path = self.resolve_chapter(&chapter_id)?;
        let _lock = crate::run::lock_job(&path)?;
        let mut job = Job::open(&path).map_err(|e| LibraryError::Manifest {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        let Some(record) = job.project.detections.iter_mut().find(|row| row.id == region_id) else {
            return Ok(None);
        };
        let source_idx = record.source_idx;
        if retype_detection(record, inside) {
            job.flush().map_err(|e| LibraryError::Manifest {
                path: path.clone(),
                detail: e.to_string(),
            })?;
            invalidate_manifest_cache(&path);
        }
        Ok(region_and_status(&chapter_id, &job.project, source_idx, region_id).map(|(region, _)| region))
    }

    /// Remove one region record entirely (manifest record + buffer files) -
    /// what "it was not there" means for a row that has no `visible` flag to
    /// turn off: the gate's and the decliner's `regions_untouched`. A patch
    /// record is soft-deleted instead ([`Library::set_mask_visible`]), because
    /// a row whose buffers are gone can never be brought back.
    pub fn remove_region(
        &self,
        region_id: &str,
    ) -> Result<Option<(ApiRegion, String)>, LibraryError> {
        let index = self.index()?;
        let Some(chapter_id) = chapter_holding(&index, region_id) else {
            return Ok(None);
        };
        let path = self.resolve_chapter(&chapter_id)?;

        let _lock = crate::run::lock_job(&path)?;
        let mut job = Job::open(&path).map_err(|e| LibraryError::Manifest {
            path: path.clone(),
            detail: e.to_string(),
        })?;

        let mut source_idx = None;
        let mut removed_artifacts = Vec::<String>::new();
        if let Some(pos) = job.project.patches.iter().position(|r| r.id == region_id) {
            let record = job.project.patches.remove(pos);
            source_idx = Some(record.source_idx);
            removed_artifacts.extend([record.mask_ref.clone(), record.ink_ref(), record.buffer_ref.clone()]);
            if record.geometry_policy == cleaner_core::text_shape::GeometryPolicy::Legacy {
                removed_artifacts.push(format!("{region_id}.ink"));
            }
        }
        job.project.text_shape_patch_revisions.retain(|record| {
            if record.id != region_id { return true; }
            removed_artifacts.extend([record.mask_ref.clone(), record.ink_ref(), record.buffer_ref.clone()]);
            false
        });
        job.project.legacy_patch_revisions.retain(|record| {
            if record.id != region_id { return true; }
            removed_artifacts.extend([record.mask_ref.clone(), record.ink_ref(), record.buffer_ref.clone()]);
            removed_artifacts.push(format!("{region_id}.ink"));
            false
        });
        job.project.text_shape_plans.retain(|plan| {
            if plan.region_id != region_id { return true; }
            removed_artifacts.extend([
                plan.base_mask_ref.clone(), plan.additions_ref.clone(), plan.removals_ref.clone(),
                plan.write_support_ref.clone(), plan.model_hole_ref.clone(),
            ]);
            if let Some(alpha) = &plan.blend_alpha_ref { removed_artifacts.push(alpha.clone()); }
            false
        });
        job.project.text_shape_corrections.retain(|entry| entry.region_id != region_id);
        // A detection is removed outright for the same reason an untouched row
        // is: there is no flag to turn off and nothing to restore.
        if let Some(pos) = job.project.detections.iter().position(|r| r.id == region_id) {
            let record = job.project.detections.remove(pos);
            source_idx = Some(record.source_idx);
            removed_artifacts.extend([record.mask_ref.clone(), record.ink_ref()]);
        }

        let prefix_idx = region_id.rfind("-u");
        if let Some(at) = prefix_idx {
            let page_id = &region_id[..at];
            let (mut gate_dropped, mut declined) = (0u32, 0u32);
            job.project.regions_untouched.retain(|record| {
                let id = format!(
                    "{}-u{}-{}-{}-{}",
                    page_id, record.bbox.x, record.bbox.y, record.bbox.w, record.bbox.h
                );
                if id == region_id {
                    source_idx = Some(record.source_idx);
                    // A held candidate (it records its balloon) was counted
                    // as neither.
                    if record.inside_bubble.is_some() {
                    } else if crate::run::is_gate_skip(&record.reason) {
                        gate_dropped += 1;
                    } else {
                        declined += 1;
                    }
                    false
                } else {
                    true
                }
            });
            let counters = &mut job.project.counters;
            counters.gate_dropped = counters.gate_dropped.saturating_sub(gate_dropped);
            counters.declined = counters.declined.saturating_sub(declined);
        }

        job.flush().map_err(|e| LibraryError::Manifest {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        removed_artifacts.sort();
        removed_artifacts.dedup();
        for reference in removed_artifacts {
            let _ = std::fs::remove_file(job.sidecar().join(reference));
        }
        invalidate_manifest_cache(&path);
        Ok(source_idx.and_then(|idx| {
            let page_index = job
                .project
                .strip
                .order
                .iter()
                .position(|index| *index == idx)?;
            let page = page_of(&chapter_id, &job.project, page_index)?;
            Some((
                ApiRegion {
                    id: region_id.to_owned(),
                    page_id: page.id.clone(),
                    source_sha: page.source_sha.clone(),
                    bbox: Bbox {
                        x: 0.0,
                        y: 0.0,
                        w: 0.0,
                        h: 0.0,
                    },
                    source: "hand",
                    detected: false,
                    padding_px: None,
                    outcome: "pending",
                    gate_skip_cause: None,
                    decline_reason: None,
                    candidate_reason: None,
                    attention: None,
                    candidate_inside_bubble: None,
                    inside_bubble: None,
                    unusually_large: false,
                    mask: None,
                },
                page.status.to_owned(),
            ))
        }))
    }

    /* ---------- building the tree ---------- */

    fn write_job(
        &self,
        manifest: &Path,
        source_dir: &Path,
        mode: StripMode,
    ) -> Result<(), LibraryError> {
        self.write_job_with_space(manifest, source_dir, mode, crate::weights::free_space)
    }

    fn write_job_with_space(
        &self,
        manifest: &Path,
        source_dir: &Path,
        mode: StripMode,
        free_space: impl Fn(&Path) -> Option<u64>,
    ) -> Result<(), LibraryError> {
        // Paths, not bytes. `ingest_paths_importing` opens one file at a time
        // and lets its buffer go before the next - a 200-page chapter is 200
        // headers in memory, never 200 pages.
        //
        // **Importing, so that a chapter owns its pages.** Every accepted file
        // is taken into the job's own `pages/` directory - a PNG or TIFF copied
        // byte for byte, a JPEG or WebP decoded once and written as a lossless
        // PNG - and that file is the page from then on. The user's file is
        // neither moved nor modified, and nothing reads it again, so the scan
        // folder can be deleted once the chapter exists. This is also still the
        // one place in the application that reads a format it cannot write, and
        // writing those formats back out is still not offered.
        //
        // What the user's folder is needed for afterwards is one question  -
        // where an export goes by default, and which files must never be
        // written over - and `converted_from` answers it without opening
        // anything.
        //
        // The cost is a second copy of the chapter on disk. That is the trade
        // the import makes, and it is why the remove-project copy had to
        // change: after an import the library's copy can be the only one left.
        let pages = sidecar_dir(manifest).join(PAGES_DIR);
        let inputs = read_directory(source_dir);
        let required = estimate_import_bytes(&inputs);
        if required > 0 {
            let available = free_space(&pages).ok_or_else(|| LibraryError::FreeSpaceUnavailable {
                path: pages.clone(),
            })?;
            if available < required {
                return Err(LibraryError::InsufficientSpace { required, available });
            }
        }
        let report = ingest::ingest_paths_importing(inputs, &pages);
        let parent = manifest.parent().unwrap_or(&self.root);
        std::fs::create_dir_all(parent).map_err(|e| LibraryError::Manifest {
            path: parent.to_path_buf(),
            detail: e.to_string(),
        })?;
        let project = Project::from_ingest(parent, env!("CARGO_PKG_VERSION"), mode, &report);
        // Under the job's own lock like every other write to a manifest in this
        // crate ([`crate::run::lock_job`]). A new chapter's job cannot be one a
        // run is inside - the id has just been minted - but the rule is "one
        // writer per job", and a rule with an exception in it is a rule
        // somebody has to remember.
        let _lock = crate::run::lock_job(manifest)?;
        Job::create(manifest, project)
            .map(|_| ())
            .map_err(|e: StoreError| LibraryError::Manifest {
                path: manifest.to_path_buf(),
                detail: e.to_string(),
            })?;
        invalidate_manifest_cache(manifest);
        Ok(())
    }

    fn build_project(&self, project: &IndexProject, at: u64) -> ApiProject {
        let mut chapters = Vec::with_capacity(project.chapters.len());
        let mut interrupted = None;
        for (order, chapter) in project.chapters.iter().enumerate() {
            // No page is resident in a listing. Home draws page counts, status
            // marks and a review total, and every one of those is answered by a
            // header or by the review index - before this parameter existed,
            // opening the app built every region of every chapter of every
            // project.
            let (api, interrupted_at) = self.build_chapter(project, chapter, order as u32, at, &[]);
            if interrupted.is_none() {
                if let Some(page_index) = interrupted_at {
                    interrupted = Some(InterruptedJob {
                        chapter_id: chapter.id.clone(),
                        page_index,
                    });
                }
            }
            chapters.push(api);
        }
        ApiProject {
            id: project.id.clone(),
            name: project.name.clone(),
            mode: project.mode,
            reading_direction: project.reading_direction,
            created: iso8601(project.created),
            app_version: env!("CARGO_PKG_VERSION"),
            source_path: project
                .source_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            last_opened: relative_time(at, project.last_opened),
            starred: project.starred,
            interrupted_job: interrupted,
            conversion: None,
            chapters,
        }
    }

    /// One chapter, and where its job stopped if it did.
    /// `resident` names the page indices whose regions are built. Empty is the
    /// listing case and it is the point of the parameter.
    fn build_chapter(
        &self,
        project: &IndexProject,
        chapter: &IndexChapter,
        order: u32,
        at: u64,
        resident: &[usize],
    ) -> (ApiChapter, Option<u32>) {
        let manifest_path = self.job_path(&project.id, &chapter.id);
        let project_manifest = self.read_manifest(&manifest_path).ok();
        let manifest = project_manifest.as_deref();

        let (pages, review) = manifest
            .map(|project| pages_and_review(&chapter.id, project, resident))
            .unwrap_or_default();
        let source_format = manifest
            .and_then(|project| project.sources.first())
            .and_then(|source| source.rel_path.extension())
            .map(|ext| ext.to_string_lossy().to_uppercase())
            .unwrap_or_default();

        (
            ApiChapter {
                id: chapter.id.clone(),
                project_id: project.id.clone(),
                name: chapter.name.clone(),
                number: chapter.number,
                order,
                last_opened: relative_time(at, chapter.last_opened),
                // The job is already open here; asking `chapter_source` would
                // parse the same manifest a second time on every listing.
                source_path: chapter
                    .source_path
                    .clone()
                    .or_else(|| manifest.and_then(|m| ingested_from_manifest(&manifest_path, m)))
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                source_format,
                no_text_detected: manifest.map(no_text_detected).unwrap_or(false),
                input_reports: manifest.map(input_reports).unwrap_or_default(),
                pages,
                review,
                denoise_replacement: manifest.and_then(crate::page_denoise::replacement),
                denoise_history: manifest.and_then(crate::denoise_history::summary),
            },
            manifest.and_then(|project| project.interrupted_at),
        )
    }
}

/// Whether this chapter has been looked at end to end and no text was found.
///
/// This needed a manifest field to answer: `counters` counts region
/// *outcomes*, and a page the detector found nothing on increments none of
/// them, so "detected nothing" and "never looked" were the same manifest.
/// [`cleaner_core::project::Project::examined`] is that field, and this is the
/// whole of what it buys at the seam.
///
/// Three clauses, and each one is load-bearing. **The whole chapter** - a
/// cancelled run that examined three of twenty pages has not established that
/// there is no text. **At least one page** - an empty chapter is not a chapter
/// with no text in it, and `strip.order` is empty before any file is ingested.
/// **Nothing found** - a gate-skipped region is text, found and deliberately
/// left alone, so it is not an empty result.
fn no_text_detected(project: &Project) -> bool {
    !project.strip.order.is_empty()
        && project
            .strip
            .order
            .iter()
            .all(|idx| project.examined.contains(idx))
        && project.patches.is_empty()
        && project.regions_untouched.is_empty()
        && project.detections.is_empty()
}

/// The notices `openChapter` staggers onto the stack, out of what ingest
/// refused. Every key already exists; none of them is composed here.
fn input_reports(project: &Project) -> Vec<NoticeSpec> {
    let report = &project.input_report;
    let mut notices = Vec::new();
    if report.junk_skipped > 0 {
        notices.push(NoticeSpec {
            key: "notice.input.junkSkipped".into(),
            params: serde_json::json!({ "count": report.junk_skipped }),
            tone: "info",
        });
    }
    // Before the refusals, because it is the one that is not a refusal: the
    // chapter opened and these pages are in it. Grouped by format for the
    // reason the skips are grouped by reason - *"{count} {from} files were
    // converted to PNG"* is only true of files that were all the same format.
    let mut by_format: Vec<(&str, usize)> = Vec::new();
    for entry in &report.converted {
        match by_format.iter_mut().find(|(from, _)| *from == entry.from) {
            Some((_, count)) => *count += 1,
            None => by_format.push((&entry.from, 1)),
        }
    }
    for (from, count) in by_format {
        notices.push(NoticeSpec {
            key: "notice.input.converted".into(),
            params: serde_json::json!({ "count": count, "from": from }),
            tone: "info",
        });
    }
    for source in &project.sources {
        if source.mode == cleaner_core::image::ColorMode::Cmyk {
            notices.push(NoticeSpec { key: "notice.input.cmykPreview".into(), params: serde_json::json!({ "file": source.rel_path.file_name().unwrap_or_default().to_string_lossy() }), tone: "info" });
        }

        if let Some(provenance) = &source.conversion {
            notices.push(NoticeSpec {
                key: "notice.input.archivedOriginal".into(),
                params: serde_json::json!({ "file": source.rel_path.file_name().unwrap_or_default().to_string_lossy(), "path": provenance.archived_original, "sha256": provenance.original_sha256 }),
                tone: "info",
            });
            if provenance.frame_count > 1 {
                notices.push(NoticeSpec { key: "notice.input.firstFrame".into(), params: serde_json::json!({ "file": source.rel_path.file_name().unwrap_or_default().to_string_lossy(), "count": provenance.frame_count }), tone: "warn" });
            }
        } else if report.converted.iter().any(|entry| source.converted_from.as_ref().and_then(|p| p.file_name()).is_some_and(|name| name.to_string_lossy() == entry.file)) {
            notices.push(NoticeSpec { key: "notice.input.missingArchive".into(), params: serde_json::json!({ "file": source.rel_path.file_name().unwrap_or_default().to_string_lossy() }), tone: "warn" });
        }
    }
    for stem in &report.duplicate_basenames {
        notices.push(NoticeSpec {
            key: "notice.input.duplicateBasename".into(),
            params: serde_json::json!({ "file": stem }),
            tone: "warn",
        });
    }
    // **One notice per reason, not one notice per chapter.**
    // Every refused file is listed, and
    // the notice's own copy is *"{count} files skipped, starting with {file} -
    // {reasonKey}"* - a sentence that is only true of files that were refused
    // for the same reason. Reporting the first refusal and the total count told
    // the user that four files were skipped because one of them was not an
    // image, which is a false sentence about three of them. Grouped, each
    // notice is true of everything it counts, and every reason the folder
    // actually produced reaches the stack.
    //
    // In first-seen order, which is natural-sort order over the folder, so the
    // stack reads in the order the files do. A `Vec` rather than a map for the
    // same reason: a handful of reasons, and the order is the point.
    let mut by_reason: Vec<(&str, &str, usize)> = Vec::new();
    for entry in &report.skipped {
        match by_reason
            .iter_mut()
            .find(|(reason, _, _)| *reason == entry.reason)
        {
            Some((_, _, count)) => *count += 1,
            None => by_reason.push((&entry.reason, &entry.file, 1)),
        }
    }
    for (reason, file, count) in by_reason {
        notices.push(NoticeSpec {
            key: "notice.input.fileSkipped".into(),
            params: serde_json::json!({ "count": count, "file": file, "reasonKey": reason }),
            tone: "warn",
        });
    }
    notices
}

/// Every page of a chapter, with regions built **only for the pages named in
/// `resident`**.
///
/// A page header is a name, a size, a hash and a status - a couple of hundred
/// bytes, flat in the number of regions on the page - and it is what the Pages
/// list, the strip's geometry and the exporter's page count all read. The
/// regions are the part that grows, they carry a provenance record each, and
/// they are wanted only for the handful of pages somebody is actually looking
/// at. An empty `resident` is the ordinary case for a *listing*: Home draws
/// twenty chapters and needs no region from any of them.
/// **One walk, two answers.** The chapter's pages and its review index are
/// built together because they are the same pass: both want every page's
/// regions constructed and then dropped, and doing it twice built every region
/// of every chapter of every project twice over on the Home screen.
fn pages_and_review(
    chapter_id: &str,
    project: &Project,
    resident: &[usize],
) -> (Vec<ApiPage>, Vec<ReviewRef>) {
    let mut pages = Vec::with_capacity(project.strip.order.len());
    let mut refs = Vec::new();
    let mut appearances = crate::tile::page_appearances(project).into_iter();
    for index in 0..project.strip.order.len() {
        let appearance = appearances.next().unwrap_or_default();
        let Some(page) = page_showing(chapter_id, project, index, appearance) else {
            continue;
        };
        for region in &page.regions {
            if let Some(reason_key) = review_reason(region) {
                refs.push(ReviewRef {
                    id: region.id.clone(),
                    page_id: page.id.clone(),
                    page_index: page.index,
                    reason_key,
                });
            }
        }
        pages.push(if resident.contains(&index) {
            page
        } else {
            shed_regions(page)
        });
    }
    (pages, refs)
}

/// One region that needs review, as the bottom bar's ⌃ ⌄ and the export
/// dialog's count read it.
///
/// Four short fields and **no region**. The review set is the one thing in the
/// interface that is honestly chapter-wide - stepping to the next issue may
/// land eleven pages away - so it cannot be derived from the resident window,
/// and deriving it from the whole chapter's regions is what kept the whole
/// chapter's regions in RAM. This is
/// the index that replaces both readings: chapter-wide, and ~90 bytes an entry.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRef {
    pub id: String,
    pub page_id: String,
    pub page_index: u32,
    /// The `review.reason.*` key, exactly as `src/lib/model/review.js` derives
    /// it from the region's own fields.
    pub reason_key: &'static str,
}

/// Why a region needs review, or `None`.
///
/// **This is `src/lib/model/review.js#reviewReason`, in Rust**, and the two are
/// pinned to each other by `the_review_index_is_review_reason_over_the_same_regions`
/// below plus the catalogue test on the other side. Two implementations rather
/// than one because the interface still applies it to a live region it is
/// holding - a run's `region-done` arrives before any index is rebuilt - and
/// this one applies it to a page it is about to drop.
///
/// The order of the clauses is review *priority* and is load-bearing: a region
/// can match more than one, and a fit failure is more actionable than a size
/// heuristic.
///
/// A detection and a candidate have none of their own: one is waiting to be
/// cleaned and the other to be chosen, and neither has had anything done to it
/// that could be wrong. Their fields carry no flag, so the clauses below
/// already say so. The one exception comes first, on any region: its cloud
/// work needs a look (`attention`, from the attempt journal).
pub(crate) fn review_reason(region: &ApiRegion) -> Option<&'static str> {
    if let Some(key) = region.attention {
        return Some(key);
    }
    if let Some(kind) = region.mask.as_ref().and_then(|mask| mask.dependency_review) {
        return Some(if kind == "changed" {
            "review.reason.inputChanged"
        } else {
            "review.reason.inputUnknown"
        });
    }
    if region
        .mask
        .as_ref()
        .is_some_and(|mask| mask.fitting_reconstructed)
    {
        return Some("review.reason.fittingReconstructed");
    }
    if region.unusually_large {
        return Some("review.reason.unusuallyLarge");
    }
    if region.mask.as_ref().is_some_and(|mask| mask.mask_quality_state.is_some())
        || region.decline_reason.as_deref() == Some("review.reason.maskNeedsCorrection")
    {
        return Some("review.reason.maskNeedsCorrection");
    }
    if let Some(key) = region.mask.as_ref().and_then(|mask| mask.mask_review) {
        return Some(key);
    }
    if region.mask.as_ref().is_some_and(|mask| mask.generated_texture_review) {
        return Some("review.reason.checkGeneratedTexture");
    }
    if region.outcome == "declined" {
        // A grouped text box with nothing under it to clean is held for what
        // it says about the mask, not because an engine failed on it.
        return Some(region.decline_reason.as_deref().and_then(mask_evidence_key)
            .unwrap_or("review.reason.declined"));
    }
    if region.outcome == "gate-skipped" {
        // An unrecognised cause still flags the region, for review.js's own
        // reason: dropping it out of review entirely is the one outcome worse
        // than naming the cause imprecisely.
        return Some(match region.gate_skip_cause {
            Some("outside-bubble") => "review.reason.gateSkippedOutsideBubble",
            Some("not-japanese") => "review.reason.gateSkippedNotJapanese",
            Some("not-text") => "review.reason.gateSkippedNotText",
            Some("language-skipped") => "review.reason.languageSkipped",
            Some("outside-language-unverified") => "review.reason.outsideLanguageUnverified",
            _ => "review.reason.gateSkippedLowConfidence",
        });
    }
    let outcome = region.mask.as_ref()?.cloud_outcome.as_ref()?;
    match outcome.rejection_cause {
        Some("safety-filter") => Some("review.reason.cloudRejectedSafetyFilter"),
        Some("transport-error") => Some("review.reason.cloudRejectedTransportError"),
        Some("parameter-test") => Some("review.reason.cloudRejectedParameterTest"),
        Some("residual-test") => Some("review.reason.cloudRejectedResidualTest"),
        Some("structural") => Some("review.reason.cloudRejectedStructural"),
        // An accepted cloud render is a cleaned layer like any other: the
        // legacy flag it once carried diagnosed nothing (see [`review_flags`]).
        _ => None,
    }
}

/// A page reduced to its header: the counts stay, the regions go.
///
/// A header still answers `status`, `region_count`, `done_count` and
/// `review_count`, because all four are questions about the page rather than
/// about its regions - and the Pages list asks all four of *every* page in the
/// chapter, not of the three in the window.
///
/// So a header is [`page_of`] with the regions shed, not a cheaper second
/// reading of the manifest rows. The counting the marks need is
/// [`review_reason`] over a built region, so a header that counted rows
/// directly would be a second opinion about what a flagged region is and would
/// drift the first time a review cause moved - the same argument the review
/// index is built on. The peak is one page's `ApiRegion`s, constructed and
/// dropped rather than kept (the same discipline applied to metadata).
fn shed_regions(mut page: ApiPage) -> ApiPage {
    page.regions = Vec::new();
    page.resident = false;
    page
}

/// The id a page carries on the seam.
///
/// One function rather than two `format!`s, because the run addresses pages by
/// id in three of its five events and the library builds the same id from the
/// manifest: two spellings that agreed today would be a page the interface
/// could not find the moment either moved.
pub(crate) fn page_id(chapter_id: &str, index: usize) -> String {
    format!("{chapter_id}-p{:03}", index + 1)
}

/// One page, out of the manifest.
///
/// Split out of [`pages_of`] because `page-done` carries a whole page and
/// rebuilding the chapter to get one of them is quadratic over a 200-page run.
pub(crate) fn page_of(chapter_id: &str, project: &Project, index: usize) -> Option<ApiPage> {
    page_showing(chapter_id, project, index, crate::tile::page_appearance(project, index))
}

/// [`page_of`] with its appearance digest already in hand: a caller building
/// many pages asks [`crate::tile::page_appearances`] once for all of them
/// rather than paying for the chapter's records once per page.
fn page_showing(chapter_id: &str, project: &Project, index: usize, appearance: String) -> Option<ApiPage> {
    let source_idx = Library::resolve_page(project, index)?;
    let mut page = blank_page(chapter_id, project, index, appearance)?;
    page.resident = true;
    let mut applied = 0usize;
    let mut deleted = 0usize;
    for record in project
        .patches
        .iter()
        .filter(|r| r.source_idx == source_idx)
    {
        // **A deleted mask is not one of the page's regions.** The row is still
        // on disk so the delete can be undone, but a listing that emitted it
        // put a maskless row back in the Layers panel and a box back on the
        // canvas the moment the user deleted one - see
        // [`region_of_deleted_patch`].
        if !record.visible {
            deleted += 1;
            continue;
        }
        applied += 1;
        let mut region = region_of_patch(record, &page);
        region.bbox = percent_of(cleaner_core::project::orientation::display_bbox(project, record), page.width, page.height);
        if let Some(mask)=region.mask.as_mut() { mask.layer=cleaner_core::project::orientation::display_layer_style(project,record); }
        // A layer is flagged when its region's cloud work needs a look too: a
        // cloud re-render whose commit went missing leaves the older layer in
        // place, and that layer is not the result the attempt committed. The
        // journal's audit already passes a layer that later work replaced on
        // purpose (a newer commit, an undo, a local clean).
        region.attention = cloud_attention_of(&record.id);
        if let Some(correction) = project.text_shape_corrections.iter()
            .find(|entry| entry.region_id == record.id && entry.source_idx == source_idx)
        {
            if let Some(mask) = region.mask.as_mut() {
                mask.mask_quality_state = Some(correction.reason.clone());
            }
        }
        page.regions.push(region);
    }
    // A page the run examined is finished with, whatever it found. Without the
    // second clause a page with no text on it would read `unclean` for ever and
    // be queued by every later run - which is the distinction
    // `Project::examined` exists to make.
    //
    // The third clause is the delete: a page whose masks have all been deleted
    // is not a cleaned page, however thoroughly the run examined it, because
    // there is nothing applied to it any more. That is the rule the mock states
    // as "if every region on the page now has no mask, the page demotes"
    // (`src/lib/api/mock.js#deleteMask`), and it says nothing about a page the
    // run found no text on - that page has no deleted rows either.
    if applied > 0 || (project.examined.contains(&source_idx) && deleted == 0) {
        page.status = "cleaned";
    }
    for record in project
        .regions_untouched
        .iter()
        .filter(|r| r.source_idx == source_idx)
    {
        let mut region = region_of_untouched(record, &page);
        let source = &project.sources[source_idx];
        region.bbox = percent_of(source.orientation.rect(record.bbox, source.w, source.h), page.width, page.height);
        if let Some(existing) = page.regions.iter_mut().find(|existing| existing.id == region.id) { *existing = region; }
        else { page.regions.push(region); }
    }
    // Detections waiting to be cleaned. A page holding any is `detected`
    // whatever else it holds: it has work left, and the Pages list marks it as
    // waiting rather than as cleaned.
    let mut detected = 0usize;
    for record in project.detections.iter().filter(|r| r.source_idx == source_idx) {
        detected += 1;
        let mut region = region_of_detection(record, &page);
        let source=&project.sources[source_idx];
        region.bbox=percent_of(source.orientation.rect(record.bbox,source.w,source.h),page.width,page.height);
        region.attention = cloud_attention_of(&record.id);
        page.regions.push(region);
    }
    if detected > 0 {
        page.status = "detected";
    }
    for correction in project.text_shape_corrections.iter()
        .filter(|entry| entry.source_idx == source_idx)
    {
        if page.regions.iter().any(|region| region.id == correction.region_id) { continue; }
        page.regions.push(ApiRegion {
            id: correction.region_id.clone(),
            page_id: page.id.clone(),
            source_sha: page.source_sha.clone(),
            bbox: percent_of(correction.candidate_bounds, page.width, page.height),
            source: "hand",
            detected: true,
            padding_px: None,
            outcome: "declined",
            gate_skip_cause: None,
            decline_reason: Some("review.reason.maskNeedsCorrection".into()),
            candidate_reason: None,
            attention: None,
            candidate_inside_bubble: None,
            inside_bubble: None,
            unusually_large: false,
            mask: None,
        });
    }
    count_page(&mut page);
    Some(page)
}

/// The page's three numbers, from its regions: how many there are, how many are
/// finished, how many need review.
///
/// One function so the header and the resident page cannot disagree - the
/// header is a page whose regions were counted and then dropped, not a second
/// opinion arrived at from the manifest rows directly.
///
/// "Finished" is masked and flagged for nothing, which is the rule
/// `editor/pagerows.js` draws the ratio and the track by: a region needing
/// review is not one the user is done with, so it does not fill the track, and
/// that is what makes the track and the ✓ mark agree.
fn count_page(page: &mut ApiPage) {
    page.region_count = page.regions.len() as u32;
    // A detection has a mask and nothing done: counted as a region, never as
    // finished.
    page.done_count = page
        .regions
        .iter()
        .filter(|region| {
            region.outcome != "detected" && region.mask.is_some() && review_reason(region).is_none()
        })
        .count() as u32;
    page.review_count = page
        .regions
        .iter()
        .filter(|region| review_reason(region).is_some())
        .count() as u32;
    page.candidate_count = page
        .regions
        .iter()
        .filter(|region| region.outcome == "candidate")
        .count() as u32;
}

/// The header every page carries, before anything has counted its regions.
fn blank_page(chapter_id: &str, project: &Project, index: usize, appearance: String) -> Option<ApiPage> {
    let source_idx = Library::resolve_page(project, index)?;
    let source = project.sources.get(source_idx)?;
    Some(ApiPage {
        id: page_id(chapter_id, index),
        chapter_id: chapter_id.to_owned(),
        index: index as u32,
        source_index: source_idx,
        number: index as u32 + 1,
        file: source
            .rel_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        source_sha: source.sha256.clone(),
        width: source.orientation.size(source.w, source.h).0,
        height: source.orientation.size(source.w, source.h).1,
        // A page with a patch on it is a cleaned page - the same rule the
        // interface applies to a live region. `cleaning` belongs to a run in
        // flight and `skipped` to a file that never became a source, so
        // neither can be read out of a manifest at rest.
        status: "unclean",
        skip_reason: None,
        regions: Vec::new(),
        region_count: 0,
        done_count: 0,
        review_count: 0,
        candidate_count: 0,
        resident: false,
        appearance,
    })
}

/// Which chapter a region id belongs to, from the id alone.
///
/// The seam addresses a region by id and sends nothing else
/// (`deleteMask` and `restoreRegion`),
/// so the chapter has to be recovered here. Every region id is built from a
/// page id and every page id from a chapter id - [`page_id`] is the one
/// spelling of that - so a chapter whose id, plus `-p`, prefixes the region id
/// is the chapter that holds it.
///
/// **Longest prefix wins.** Chapter ids are minted from one counter and are not
/// prefix-free: `ch1` prefixes `ch12-p001-r0` as surely as `ch12` does, and the
/// shorter match would send the write to the wrong manifest. Index entries
/// only, no manifest is opened.
pub(crate) fn chapter_holding(index: &Index, region_id: &str) -> Option<String> {
    let mut best: Option<&str> = None;
    for project in &index.projects {
        for chapter in &project.chapters {
            if !region_id.starts_with(&format!("{}-p", chapter.id)) {
                continue;
            }
            if best
                .map(|current| chapter.id.len() > current.len())
                .unwrap_or(true)
            {
                best = Some(&chapter.id);
            }
        }
    }
    best.map(str::to_owned)
}

/// One region as the manifest now holds it, with its page's status.
///
/// The pair every region-level edit answers with: some edits move the page -
/// deleting the last mask on one takes it back out of "cleaned" - and a caller
/// that replaced only the region would draw a full track under a mark that
/// disagrees with it.
pub(crate) fn region_and_status(
    chapter_id: &str,
    project: &Project,
    source_idx: usize,
    region_id: &str,
) -> Option<(ApiRegion, String)> {
    // `page_of` takes a position in `strip.order`, never a source index -
    // `Library::resolve_page` is the rule, and this is it read backwards.
    let page_index = project
        .strip
        .order
        .iter()
        .position(|index| *index == source_idx)?;
    let page = page_of(chapter_id, project, page_index)?;
    if let Some(region) = page.regions.iter().find(|region| region.id == region_id) {
        return Some((region.clone(), page.status.to_owned()));
    }
    // A deleted mask is no longer one of the page's regions, and the edit that
    // deleted it still has to answer with what it did - so it is built from the
    // record directly. Only for a record that is *there and invisible*: an id
    // nothing names is still `None`, which is the answer the seam gives for a
    // region it cannot find.
    let record = project
        .patches
        .iter()
        .find(|r| r.id == region_id && !r.visible)?;
    let mut region = region_of_deleted_patch(record, &page);
    region.bbox = percent_of(cleaner_core::project::orientation::display_bbox(project, record), page.width, page.height);
    Some((region, page.status.to_owned()))
}

/// The folder a job's sources sit in, read out of the manifest.
///
/// The manifest holds paths relative to its own directory, so the join is what
/// turns them back into somewhere on disk, and the result runs through
/// `canonicalize` because that join produces a long `..` chain from an app-data
/// manifest to a folder in `~/scans` - true, and not a path to show anyone or
/// to compare against one a user typed.
fn ingested_from_manifest(manifest_path: &Path, project: &Project) -> Option<PathBuf> {
    let source = project.sources.first()?;
    let root = manifest_path.parent().unwrap_or(Path::new("."));
    // `converted_from` where there is one: a converted page's own file sits in
    // the job's sidecar, and answering "where did this chapter come from" with
    // a directory inside the application's library is answering a different
    // question than the one asked.
    let rel = source.converted_from.as_ref().unwrap_or(&source.rel_path);
    let dir = root.join(rel).parent()?.to_path_buf();
    Some(std::fs::canonicalize(&dir).unwrap_or(dir))
}

/// Whether two paths name the same directory.
///
/// Textual equality first, because that is the common case and it needs no
/// filesystem; `canonicalize` after it, because the paths being compared are
/// reached two different ways - one built from what the user typed, one joined
/// out of a manifest - and the same folder written two ways is still one folder
/// that must not be handed to two chapters. Two paths that cannot be
/// canonicalised are not the same folder: a directory that is not there is not
/// one another chapter is reading.
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(left), Ok(right)) if left == right
        )
}

/// Every file directly in a directory, **by name**. Sub-directories are not
/// descended into: `ingest` is given one chapter's worth of files, and a
/// recursive walk would make a project folder holding twelve chapter
/// directories into one 300-page chapter.
///
/// Names rather than bytes, deliberately. Handing `ingest` a
/// `Vec<(PathBuf, Vec<u8>)>` put every page of a chapter in memory at once,
/// which is the exact peak [`cleaner_core::ingest::SourceRef`] is header-only
/// to avoid; `ingest_paths` does the reading, one file at a time. A file that
/// cannot be opened is now listed as a refusal by ingest rather than dropped
/// here without a word.
fn estimate_import_bytes(paths: &[PathBuf]) -> u64 {
    let mut bytes = 0u64;
    for path in paths {
        if ingest::is_junk(path) {
            continue;
        }
        let Ok(source) = std::fs::read(path) else { continue };
        if cleaner_core::image::Format::sniff(&source).is_some() {
            if ingest::source_ref(path, &source).is_ok() {
                bytes = bytes.saturating_add(source.len() as u64);
            }
        } else if let Ok(raster) = cleaner_core::image::foreign::decode(&source) {
            // Measure the same PNG or TIFF the importer writes, so compressed
            // scans do not inherit a 16-bit RGBA estimate.
            if let Some(expanded) = estimate_converted_bytes(&raster) {
                bytes = bytes.saturating_add(expanded).saturating_add(source.len() as u64);
            }
        }
    }
    if bytes == 0 { 0 } else { bytes.saturating_add(1_048_576) }
}

fn estimate_converted_bytes(raster: &cleaner_core::image::Raster) -> Option<u64> {
    let target = cleaner_core::image::lossless_format_for(raster);
    cleaner_core::image::encode(raster, target)
        .ok()
        .map(|encoded| (encoded.len() as u64).saturating_add(1_048_576))
}

fn read_directory(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect()
}

fn take_id(index: &mut Index) -> u64 {
    let id = index.next_id.max(1);
    index.next_id = id + 1;
    id
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

/// Resolve a chapter id to its `.mtclean` job path.
///
/// The entry point other command modules use - `exportChapter`, `tile://`, and
/// the run when it lands. Kept as a free function taking an `AppHandle` so a
/// caller needs to know nothing about the library's layout.
///
/// `allow(dead_code)` because the callers are sibling modules that are wired in
/// `lib.rs` independently of this one; a build with only the library in it
/// reads this as unused.
#[allow(dead_code)]
pub fn resolve_chapter(
    app: &tauri::AppHandle,
    chapter_id: &str,
) -> Result<std::path::PathBuf, LibraryError> {
    Library::for_app(app)?.resolve_chapter(chapter_id)
}

/// Run a command's body on the blocking executor, and answer when it is done.
///
/// **Every command in this crate that touches the filesystem goes through
/// this**, because of how `tauri-macros` expands `#[tauri::command]`. A
/// function without `async` is expanded through `body_blocking`, which calls it
/// *inline* on the thread the invoke handler arrives on - the UI thread. A
/// whole-folder ingest, a re-parse of every manifest in the library, or an
/// export's composite-and-write would hold the webview for its whole duration,
/// and the dialog's own busy spinner could not paint the frame that says so.
/// A function *with* `async` is expanded through `respond_async_serialized`,
/// which is `async_runtime::spawn`.
///
/// So the fix is `async`, and the question is only where the blocking work then
/// runs. `spawn` puts it on a tokio worker, where a multi-second ingest stalls
/// every other task spawned on the runtime - which will include the run
/// scheduler and its event emission. `spawn_blocking` is the executor meant for
/// exactly this, so that is where it goes.
///
/// Verified against tauri 2.11.5 / tauri-macros 2.6.3, the versions
/// `src-tauri/Cargo.toml` resolves to: `command/wrapper.rs`'s `body_blocking`
/// against `body_async`, and `ipc/mod.rs`'s `respond_async_serialized_inner`.
pub(crate) async fn blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    // A panic in the body reaches the seam as a failed call rather than as a
    // promise that never settles.
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(error.to_string()),
    }
}

#[tauri::command]
pub async fn list_projects(app: tauri::AppHandle) -> Result<Vec<ApiProject>, String> {
    blocking(move || Ok(Library::for_app(&app)?.list_projects()?)).await
}

#[tauri::command]
pub async fn create_project(
    app: tauri::AppHandle,
    name: String,
    mode: StripMode,
    source_path: Option<String>,
    reading_direction: Option<ReadingDirection>,
) -> Result<ApiProject, String> {
    let project = blocking(move || {
        Ok(Library::for_app(&app)?.create_project(
            &name,
            mode,
            source_path.map(PathBuf::from),
            reading_direction,
        )?)
    })
    .await?;
    // The announcement is the command's, not the model's: `Library` is written
    // to be testable without a Tauri runtime, and a global emit inside it would
    // leak between unit tests.
    crate::events::notice(
        "notice.project.created",
        serde_json::json!({ "modeKey": mode_key(project.mode) }),
        "info",
    );
    Ok(project)
}

/// The `project.mode.*` key a mode carries onto the notice stack. The
/// catalogue's own names, not the manifest's.
fn mode_key(mode: StripMode) -> &'static str {
    match mode {
        StripMode::Single => "project.mode.single",
        StripMode::Longstrip => "project.mode.longstrip",
    }
}

/// `createChapter`, with the source folder the seam carries.
///
/// `source_path` is optional on the seam and stays optional here: a project that
/// points at a folder of scans should not make a user re-state it for the first
/// chapter. What the command adds is the answer to the case the option leaves
/// open - a second chapter that would have silently inherited the first one's
/// folder is refused, and the refusal is a notice naming the chapter that
/// already reads it, because the user is the only one who knows where the other
/// chapter's pages actually are.
#[tauri::command]
pub async fn create_chapter(
    app: tauri::AppHandle,
    project_id: String,
    name: String,
    number: Option<u32>,
    source_path: Option<String>,
) -> Result<Option<ApiChapter>, String> {
    let handle = app.clone();
    let id = project_id.clone();
    let outcome = blocking(move || {
        Ok(Library::for_app(&handle)?.create_chapter(
            &id,
            &name,
            number,
            source_path.map(PathBuf::from),
        ))
    })
    .await?;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error @ (LibraryError::InsufficientSpace { .. } | LibraryError::FreeSpaceUnavailable { .. })) => {
            crate::events::notice(error.reason_key(), serde_json::json!({}), "warn");
            return Ok(None);
        }
        Err(error) => return Err(error.to_string()),
    };
    match outcome {
        NewChapter::NoProject => Ok(None),
        NewChapter::SourceTaken { chapter } => {
            crate::events::notice(
                "notice.chapter.sourceTaken",
                serde_json::json!({ "chapter": chapter }),
                "warn",
            );
            Ok(None)
        }
        NewChapter::Created(chapter) => {
            let project_name = Library::for_app(&app)
                .and_then(|library| library.index())
                .ok()
                .and_then(|index| {
                    index
                        .projects
                        .iter()
                        .find(|p| p.id == project_id)
                        .map(|p| p.name.clone())
                })
                .unwrap_or_default();
            crate::events::notice(
                "notice.chapter.added",
                serde_json::json!({ "chapter": chapter.number, "project": project_name }),
                "info",
            );
            Ok(Some(*chapter))
        }
    }
}

#[tauri::command]
pub async fn open_chapter(
    app: tauri::AppHandle,
    project_id: String,
    chapter_id: String,
    convert: Option<bool>,
) -> Result<Option<OpenedChapter>, String> {
    let opened = blocking(move || {
        Ok(Library::for_app(&app)?.open_chapter(
            &project_id,
            &chapter_id,
            convert.unwrap_or(false),
        )?)
    })
    .await?;
    if let Some(opened) = &opened {
        // The input report was persisted so that this could happen; until the
        // channel existed it was persisted and then dropped on the floor.
        // Staggered because the stack is a queue a user
        // reads, which is why the mock staggers it.
        let mut notices: Vec<(String, serde_json::Value, &'static str)> = opened
            .chapter
            .input_reports
            .iter()
            .map(|report| (report.key.clone(), report.params.clone(), report.tone))
            .collect();
        if opened.chapter.no_text_detected {
            notices.push((
                "notice.chapter.emptyResult".into(),
                serde_json::json!({ "regions": 0, "pages": opened.chapter.pages.len() }),
                "warn",
            ));
        }
        crate::events::stagger(notices);
    }
    Ok(opened)
}

/// Bring a window of pages into residency.
///
/// Silent: it is paging, not an edit, and a notice per page turn would bury
/// every notice that carries information.
#[tauri::command]
pub async fn load_pages(
    app: tauri::AppHandle,
    chapter_id: String,
    indices: Vec<usize>,
) -> Result<Vec<ApiPage>, String> {
    blocking(move || Ok(Library::for_app(&app)?.load_pages(&chapter_id, &indices)?)).await
}

#[tauri::command]
pub async fn rename_project(
    app: tauri::AppHandle,
    project_id: String,
    name: String,
) -> Result<Option<ApiProject>, String> {
    let announced = name.clone();
    let project =
        blocking(move || Ok(Library::for_app(&app)?.rename_project(&project_id, &name)?)).await?;
    if project.is_some() {
        crate::events::notice(
            "notice.project.renamed",
            serde_json::json!({ "name": announced }),
            "info",
        );
    }
    Ok(project)
}

#[tauri::command]
pub async fn delete_project(app: tauri::AppHandle, project_id: String) -> Result<bool, String> {
    let handle = app.clone();
    let id = project_id.clone();
    // Read the name before the row goes: the notice says which project left the
    // library, and after the delete there is nothing left to ask.
    let name = Library::for_app(&app)
        .and_then(|library| library.index())
        .ok()
        .and_then(|index| {
            index
                .projects
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
        })
        .unwrap_or_default();
    let deleted =
        blocking(move || Ok(Library::for_app(&handle)?.delete_project(&project_id)?)).await?;
    if deleted {
        crate::events::notice(
            "notice.project.deleted",
            serde_json::json!({ "name": name }),
            "warn",
        );
    }
    Ok(deleted)
}

/// Delete one chapter - the row, its manifest, its masks and patches, and, when
/// the dialog asked for it, the folder of scans it was read from.
///
/// The notice says which of those happened to the scans rather than reporting a
/// flat "deleted": the user chose, and a refusal to empty the project's own
/// folder is a refusal they have to be told about.
#[tauri::command]
pub async fn delete_chapter(
    app: tauri::AppHandle,
    project_id: String,
    chapter_id: String,
    source_files: bool,
) -> Result<bool, String> {
    let outcome = blocking(move || {
        Ok(Library::for_app(&app)?.delete_chapter(&project_id, &chapter_id, source_files)?)
    })
    .await?;
    match outcome {
        ChapterDeleted::NoChapter => Ok(false),
        ChapterDeleted::Deleted { number, scans } => {
            let key = match scans {
                Scans::Removed => "notice.chapter.deletedWithScans",
                Scans::KeptProjectFolder => "notice.chapter.deletedProjectFolderKept",
                Scans::KeptUnknown => "notice.chapter.deletedSourceUnknown",
                Scans::Kept => "notice.chapter.deleted",
            };
            crate::events::notice(key, serde_json::json!({ "chapter": number }), "warn");
            Ok(true)
        }
    }
}

/// The region id inside a mask id.
///
/// `ApiMask.id` is `<region id>-m<n>` and the shape is load-bearing - the
/// comment on the field says so, and this is the reader that depends on it.
pub(crate) fn region_of_mask(mask_id: &str) -> &str {
    match mask_id.rfind("-m") {
        Some(at) if mask_id[at + 2..].chars().all(|c| c.is_ascii_digit()) => &mask_id[..at],
        _ => mask_id,
    }
}

#[derive(Clone)]
enum ObservedMask {
    Detection(Box<cleaner_core::project::DetectedRegion>),
    Patch(Box<cleaner_core::project::PatchRecord>),
}

impl Library {
    /// Capture the row named by the user's mask before the deletion is queued.
    /// The atomic manifest read gives one complete revision even while a run
    /// is writing; the mutation below compares again under the job lock.
    fn observe_mask(&self, region_id: &str) -> Result<Option<ObservedMask>, LibraryError> {
        let index = self.index()?;
        let Some(chapter_id) = chapter_holding(&index, region_id) else { return Ok(None) };
        let path = self.resolve_chapter(&chapter_id)?;
        let job = Job::open(&path).map_err(|e| LibraryError::Manifest {
            path: path.clone(), detail: e.to_string(),
        })?;
        Ok(job.project.detections.iter().find(|row| row.id == region_id)
            .cloned().map(Box::new).map(ObservedMask::Detection)
            .or_else(|| job.project.patches.iter().find(|row| row.id == region_id)
                .cloned().map(Box::new).map(ObservedMask::Patch)))
    }

    fn delete_observed_mask(&self, region_id: &str, observed: ObservedMask)
        -> Result<Option<(ApiRegion, String)>, String> {
        let index = self.index().map_err(|e| e.to_string())?;
        let chapter_id = chapter_holding(&index, region_id).ok_or("mask_changed")?;
        let path = self.resolve_chapter(&chapter_id).map_err(|e| e.to_string())?;
        let _lock = crate::run::lock_job(&path)?;
        let mut job = Job::open(&path).map_err(|e| e.to_string())?;
        match observed {
            ObservedMask::Detection(before) => {
                if job.project.detections.iter().find(|row| row.id == region_id) != Some(&*before) {
                    return Err("mask_changed".into());
                }
                let record = job.remove_detection(region_id).map_err(|e| e.to_string())?.ok_or("mask_changed")?;
                invalidate_manifest_cache(&path);
                let Some(page_index) = job.project.strip.order.iter().position(|index| *index == record.source_idx) else { return Ok(None) };
                let Some(page) = page_of(&chapter_id, &job.project, page_index) else { return Ok(None) };
                Ok(Some((region_of_detection(&record, &page), page.status.to_owned())))
            }
            ObservedMask::Patch(before) => {
                let Some(record) = job.project.patches.iter_mut().find(|row| row.id == region_id) else {
                    return Err("mask_changed".into());
                };
                if *record != *before { return Err("mask_changed".into()); }
                record.visible = false;
                let source_idx = record.source_idx;
                job.flush().map_err(|e| e.to_string())?;
                crate::underlay::refresh_dependencies(&mut job, region_id)?;
                invalidate_manifest_cache(&path);
                Ok(region_and_status(&chapter_id, &job.project, source_idx, region_id))
            }
        }
    }
}

/// What `deleteMask` answers with: **no region**, and the page's status.
///
/// `region: null` is the whole of the change the user asked for. Deleting a
/// mask takes the region off the page - out of the Layers panel, off the
/// canvas, out of the page's counts - so there is no region to hand back, and
/// handing back a maskless one is what left an empty row behind. The field
/// stays, spelled `null`, because the seam declares
/// the answer as `{region, pageStatus}` and every caller reads both halves.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedMask {
    pub region: Option<ApiRegion>,
    pub page_status: String,
}

/// Delete a region's mask. The original text under it comes back, and the
/// region goes with it.
///
/// The patch's row and its buffers stay on disk with `visible` turned off -
/// see [`region_of_deleted_patch`] for why a soft delete is the only kind the
/// seam's undo can be served from - but nothing lists an invisible record, so
/// what the user sees is the region gone.
#[tauri::command]
pub async fn delete_mask(
    app: tauri::AppHandle,
    mask_id: String,
) -> Result<Option<DeletedMask>, String> {
    let region_id = region_of_mask(&mask_id).to_owned();
    let observed = Library::for_app(&app).map_err(|e| e.to_string())?
        .observe_mask(&region_id).map_err(|e| e.to_string())?;
    let edited = blocking(move || {
        let library = Library::for_app(&app)?;
        match observed {
            Some(observed) => library.delete_observed_mask(&region_id, observed),
            None => Ok(None),
        }
    })
    .await?;
    if edited.is_some() {
        crate::events::notice("notice.mask.deleted", serde_json::json!({}), "info");
    }
    Ok(edited.map(|(_, page_status)| DeletedMask {
        region: None,
        page_status,
    }))
}

/// Put a region back as the caller last saw it - the undo half of every
/// region-level edit.
///
/// This build serves the half it can: a mask this command deleted goes back on
/// (`region` present), and a region the caller says was not there goes off
/// again (`region: null`). The `region` payload is not written into the
/// manifest, and deliberately so - the manifest's own record is still there and
/// is the better copy. What the caller is really asking is *which of the two
/// states*, and `null` against not-null carries that.
///
/// Silent, like the mock: the undo control is the feedback, and a notice per
/// undo would bury the ones that carry information.
#[tauri::command]
pub async fn restore_region(
    app: tauri::AppHandle,
    region_id: String,
    region: Option<serde_json::Value>,
) -> Result<Option<ApiRegion>, String> {
    let wanted = !matches!(region, None | Some(serde_json::Value::Null));
    let revision = region.as_ref().and_then(|snapshot| {
        snapshot.pointer("/mask/textShapePatchRevision")
            .or_else(|| snapshot.pointer("/mask/textShapePlanIdentity"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    });
    let legacy_revision = region.as_ref().and_then(|snapshot| {
        snapshot.pointer("/mask/legacyPatchRevision")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    });
    let layer = region.as_ref()
        .and_then(|snapshot| snapshot.pointer("/mask/layer"))
        .and_then(|value| serde_json::from_value::<cleaner_core::patch::LayerStyle>(value.clone()).ok());
    let edited = blocking(move || {
        let library = Library::for_app(&app)?;
        if wanted {
            let restored = if let Some(revision) = revision {
                library.restore_text_shape_mask(&region_id, &revision)?
            } else if let Some(revision) = legacy_revision {
                library.restore_legacy_mask(&region_id, &revision)?
            } else {
                library.set_mask_visible(&region_id, true)?
            };
            if let Some(layer) = layer {
                return Ok(library.set_layer_style(&region_id, layer, LayerEdit::Restore)?.or(restored));
            }
            return Ok(restored);
        }
        // **"It was not there" is the soft delete, when there is a patch to
        // soften.** This is the redo of a `deleteMask` as often as it is the
        // undo of a hand-drawn creation, and the two arrive as the same call -
        // a snapshot with no region. Removing the row and its buffers would
        // serve the second and break the first: the undo that follows a redo
        // has nothing left to turn back on. So a patch record goes invisible,
        // exactly as `deleteMask` leaves it, and a hard removal is kept for the
        // rows that have no `visible` flag to turn off - the gate's and the
        // decliner's `regions_untouched`.
        match library.set_mask_visible(&region_id, false)? {
            Some(edited) => Ok(Some(edited)),
            None => Ok(library.remove_region(&region_id)?),
        }
    })
    .await?;
    // A removal answers with nothing, as the seam says: there is no region to
    // hand back, and the caller's own snapshot is what it applies.
    Ok(if wanted {
        edited.map(|(region, _)| region)
    } else {
        None
    })
}

/// Change one layer's opacity, position, rotation or lock - the layer
/// controls and the canvas gestures, never history (that is
/// `restore_region`).
///
/// Refused, with the refusal's catalogue key as the error, when the layer's
/// capabilities do not allow the change: see [`Library::set_layer_style`].
/// A region that is not there answers `null`, as every region edit does.
#[tauri::command]
pub async fn set_layer_style(
    app: tauri::AppHandle,
    region_id: String,
    layer: serde_json::Value,
) -> Result<Option<ApiRegion>, String> {
    let layer = serde_json::from_value::<cleaner_core::patch::LayerStyle>(layer)
        .map_err(|error| format!("layer style: {error}"))?;
    let edited = blocking(move || {
        Ok(Library::for_app(&app)?.set_layer_style(&region_id, layer, LayerEdit::Edit)?)
    })
    .await?;
    Ok(edited.map(|(region, _)| region))
}

#[tauri::command]
pub async fn keep_dependency_result(
    app: tauri::AppHandle,
    region_id: String,
) -> Result<Option<ApiRegion>, String> {
    blocking(move || Ok(Library::for_app(&app)?.keep_dependency_result(&region_id)?)).await
}

/// `setDetectionType`: whether a detected region's text is inside a speech
/// balloon, set from the region menu before the region is cleaned
/// ([`Library::set_detection_inside`]). Answers the region as it now is.
///
/// Rejects with `not_a_detection` for an id no stored detection carries, a
/// region cleaned or deleted since the menu opened among them, rather than
/// answering as though the type had been set. Not undoable, as a hand edit of
/// a detection's mask is not (`edit_detection_mask`). Picking the other type
/// again sets the type back; the measured balloon colour stays cleared, and a
/// pick moved to `lama` stays there.
#[tauri::command]
pub async fn set_detection_type(
    app: tauri::AppHandle,
    region_id: String,
    inside: bool,
) -> Result<ApiRegion, String> {
    blocking(move || {
        Library::for_app(&app)?
            .set_detection_inside(&region_id, inside)?
            .ok_or_else(|| format!("not_a_detection: {region_id} is not a stored detection"))
    })
    .await
}

impl From<LibraryError> for String {
    fn from(error: LibraryError) -> String {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::image::{encode, fixtures, Format};
    use cleaner_core::mask::Mask;
    use cleaner_core::patch::{Patch, Provenance};

    /// A scratch directory that removes itself - the same shape the project
    /// store's tests use, and for the same reason: a unique path, not a
    /// dependency.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir()
                .join("manga-cleaner-library")
                .join(format!("{name}-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch directory");
            Scratch(dir)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A folder of JPEG scans: `count` distinct pages, naturally ordered.
    ///
    /// A real encoder rather than a checked-in blob, for the reason
    /// `cleaner_core::image::foreign`'s own tests give: what is under test is
    /// that ingest reads what a scanner writes.
    fn jpeg_scans(dir: &Path, count: usize) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        for n in 1..=count {
            let raster = &fixtures::by_name("rgb8").raster;
            let mut samples = raster.data.clone();
            // Distinct pixels per page, or ingest deduplicates them - and the
            // difference has to survive a lossy encoder, so it is a whole row
            // rather than one byte.
            for byte in samples.iter_mut().take(raster.stride()) {
                *byte = (n * 40) as u8;
            }
            let buffer = image::RgbImage::from_raw(raster.width, raster.height, samples).unwrap();
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgb8(buffer)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Jpeg,
                )
                .unwrap();
            std::fs::write(dir.join(format!("{n:03}.jpg")), bytes).unwrap();
        }
        dir.to_path_buf()
    }

    /// A folder of scans: `count` distinct PNG pages, naturally ordered.
    fn scans(dir: &Path, count: usize) -> PathBuf {
        tagged_scans(dir, count, 0)
    }

    /// The same, with every page in the folder carrying `tag` - so two folders
    /// built this way hold files no two of which are byte-identical.
    fn tagged_scans(dir: &Path, count: usize, tag: u8) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        for n in 1..=count {
            let mut raster = fixtures::by_name("l8").raster;
            // Distinct bytes per page, or ingest deduplicates them.
            raster.data[0] = n as u8;
            raster.data[1] = tag;
            let bytes = encode(&raster, Format::Png).unwrap();
            std::fs::write(dir.join(format!("{n:03}.png")), bytes).unwrap();
        }
        dir.to_path_buf()
    }

    fn library(scratch: &Scratch) -> Library {
        Library::at(scratch.join("library"))
    }

    fn a_project(library: &Library, scratch: &Scratch, pages: usize) -> ApiProject {
        let source = scans(&scratch.join("raws"), pages);
        library
            .create_project("Wandering Moon", StripMode::Single, Some(source), None)
            .unwrap()
    }

    #[test]
    fn unsafe_text_shape_plan_stays_in_review_after_reopen() {
        let scratch = Scratch::new("shape-correction-reopen");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let mut job = Job::open(&path).unwrap();
        job.record_text_shape_correction(
            0, "shape-draft", 3, Rect::new(2, 3, 4, 5), "empty base mask",
        ).unwrap();
        drop(job);
        let reopened = Job::open(&path).unwrap();
        let page = page_of(&chapter.id, &reopened.project, 0).unwrap();
        let region = page.regions.iter().find(|region| region.id == "shape-draft").unwrap();
        assert_eq!(region.outcome, "declined");
        assert_eq!(region.decline_reason.as_deref(), Some("review.reason.maskNeedsCorrection"));
        assert_eq!(region.bbox, percent_of(Rect::new(2, 3, 4, 5), page.width, page.height));
        assert_eq!(page.review_count, 1);
    }

    fn provenance(engine: Engine) -> Provenance {
        Provenance {
            engine,
            engine_version: "test".into(),
            model_sha256: None,
            execution_provider: "cpu".into(),
            params_snapshot: serde_json::json!({}),
            mask_sha256: "0".repeat(64),
            source_sha256: "1".repeat(64),
            cloud: None,
            created: 1_760_000_000,
        }
    }

    fn a_patch(id: &str, bounds: Rect, engine: Engine) -> Patch {
        let mut pixels = fixtures::by_name("l8").raster;
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.data = vec![200; (bounds.w * bounds.h) as usize];
        Patch {
            id: id.into(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 0,
            visible: true,
            provenance: provenance(engine),
        }
    }

    #[test]
    fn chapter_import_refuses_full_volume_before_writing() {
        let scratch = Scratch::new("ingest-full-volume");
        let library = library(&scratch);
        let source = scans(&scratch.join("scans"), 2);
        let manifest = scratch.join("library/chapter.mtclean");
        let error = library.write_job_with_space(&manifest, &source, StripMode::Single, |_| Some(0))
            .unwrap_err();
        assert!(matches!(error, LibraryError::InsufficientSpace { required: 1.., available: 0 }));
        assert_eq!(error.reason_key(), "notice.library.ingestInsufficientSpace");
        assert!(!manifest.exists());
        assert!(!sidecar_dir(&manifest).exists());
    }

    #[test]
    fn dependency_keep_preserves_pixels_and_unrelated_edits_leave_b_alone() {
        let scratch = Scratch::new("dependency-keep");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let a_id = format!("{}-r1", page_id(&chapter.id, 0));
        let b_id = format!("{}-r2", page_id(&chapter.id, 0));
        let mut job = Job::open(&path).unwrap();
        let a = a_patch(&a_id, Rect::new(0, 0, 4, 4), Engine::Fill);
        job.complete_region(0, &a, None).unwrap();
        let mut b = a_patch(&b_id, Rect::new(40, 30, 4, 4), Engine::Fill);
        b.order = 1;
        b.provenance.params_snapshot["input_provenance"] = serde_json::json!({
            "read_footprint": Rect::new(35, 25, 12, 12),
            "input_sha256": "unrelated-digest"
        });
        job.complete_region(0, &b, None).unwrap();
        crate::underlay::refresh_dependencies(&mut job, &a_id).unwrap();
        assert_eq!(job.project.patches[1].review_state, None);

        job.project.patches[1].review_state = Some("review.reason.inputChanged".into());
        job.project.patches[1].provenance.params_snapshot["review_before_input_change"] =
            serde_json::Value::Null;
        job.flush().unwrap();
        let before = job.load_patch(&job.project.patches[1]).unwrap().pixels.data;
        drop(job);
        let kept = library.keep_dependency_result(&b_id).unwrap().unwrap();
        assert_eq!(kept.mask.unwrap().dependency_review, None);
        let reopened = Job::open(&path).unwrap();
        assert_eq!(reopened.project.patches[1].review_state, None);
        assert_eq!(reopened.load_patch(&reopened.project.patches[1]).unwrap().pixels.data, before);
    }

    #[test]
    fn legacy_restore_selects_old_and_new_pixels_after_reopen() {
        let scratch = Scratch::new("legacy-restore-reopen");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let region_id = format!("{}-r1", page_id(&chapter.id, 0));
        let mut job = Job::open(&path).unwrap();
        let mut patch = a_patch(&region_id, Rect::new(10, 10, 4, 4), Engine::Fill);
        patch.pixels.data.fill(17);
        job.complete_region(0, &patch, None).unwrap();
        let old = job.project.patches[0].legacy_revision_id().unwrap();
        patch.pixels.data.fill(201);
        job.complete_region(0, &patch, None).unwrap();
        let new = job.project.patches[0].legacy_revision_id().unwrap();
        drop(job);

        library.restore_legacy_mask(&region_id, &old).unwrap().unwrap();
        let reopened = Job::open(&path).unwrap();
        assert!(reopened.load_patch(&reopened.project.patches[0]).unwrap().pixels.data.iter().all(|&p| p == 17));
        drop(reopened);
        library.restore_legacy_mask(&region_id, &new).unwrap().unwrap();
        let reopened = Job::open(&path).unwrap();
        assert!(reopened.load_patch(&reopened.project.patches[0]).unwrap().pixels.data.iter().all(|&p| p == 201));
    }

    #[test]
    fn hard_remove_deletes_legacy_rerun_sidecars_but_keeps_other_regions() {
        let scratch = Scratch::new("legacy-hard-remove");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let region_id = format!("{}-r1", page_id(&chapter.id, 0));
        let other_id = format!("{}-r2", page_id(&chapter.id, 0));
        let mut job = Job::open(&path).unwrap();
        let mut patch = a_patch(&region_id, Rect::new(10, 10, 4, 4), Engine::Fill);
        for value in [17, 91, 201] {
            patch.pixels.data.fill(value);
            job.complete_region(0, &patch, None).unwrap();
        }
        job.complete_region(0, &a_patch(&other_id, Rect::new(30, 30, 4, 4), Engine::Fill), None)
            .unwrap();
        assert_eq!(job.project.legacy_patch_revisions.iter().filter(|row| row.id == region_id).count(), 3);
        let mut removed_files: Vec<_> = job.project.patches.iter()
            .chain(job.project.legacy_patch_revisions.iter())
            .filter(|row| row.id == region_id)
            .flat_map(|row| [row.mask_ref.clone(), row.ink_ref(), row.buffer_ref.clone()])
            .collect();
        removed_files.push(format!("{region_id}.ink"));
        let other = job.project.patches.iter().find(|row| row.id == other_id).unwrap();
        let other_files = [other.mask_ref.clone(), other.ink_ref(), other.buffer_ref.clone()];
        assert!(removed_files.iter().all(|name| job.sidecar().join(name).exists()));
        assert!(other_files.iter().all(|name| job.sidecar().join(name).exists()));
        let sidecar = job.sidecar().to_path_buf();
        drop(job);

        assert!(library.remove_region(&region_id).unwrap().is_some());
        let reopened = Job::open(&path).unwrap();
        assert!(!reopened.project.patches.iter().any(|row| row.id == region_id));
        assert!(!reopened.project.legacy_patch_revisions.iter().any(|row| row.id == region_id));
        assert!(removed_files.iter().all(|name| !sidecar.join(name).exists()));
        assert!(reopened.project.patches.iter().any(|row| row.id == other_id));
        assert!(other_files.iter().all(|name| sidecar.join(name).exists()));
    }

    /* ---------- the index ---------- */

    #[test]
    fn a_library_that_has_never_been_written_is_empty_rather_than_an_error() {
        let scratch = Scratch::new("first-launch");
        let library = library(&scratch);
        assert!(library.list_projects().unwrap().is_empty());
        assert!(
            !library.index_path().exists(),
            "reading the library created it"
        );
    }

    /// A writer that skips the index lock (an older build) and saves between
    /// this one's read and its save is not overwritten: the save is refused,
    /// and what that writer wrote stays.
    #[test]
    fn an_index_changed_after_it_was_read_is_not_overwritten() {
        let scratch = Scratch::new("index-stale");
        let library = library(&scratch);
        a_project(&library, &scratch, 1);

        let mut index = library.index().unwrap();
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(library.index_path()).unwrap()).unwrap();
        raw["next_id"] = serde_json::json!(999);
        std::fs::write(library.index_path(), serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        index.projects[0].name = "Renamed".into();
        assert!(matches!(library.save(&mut index), Err(LibraryError::Index { .. })));
        let on_disk = library.index().unwrap();
        assert_eq!(on_disk.next_id, 999, "the other writer's index was overwritten");
        assert_ne!(on_disk.projects[0].name, "Renamed");

        // One read saved twice is one writer, not two.
        let mut index = library.index().unwrap();
        library.save(&mut index).unwrap();
        library.save(&mut index).unwrap();

        // And a library with no index yet is not one to write over another
        // writer's first index.
        let fresh = Library::at(scratch.join("fresh"));
        let mut empty = fresh.index().unwrap();
        std::fs::create_dir_all(scratch.join("fresh")).unwrap();
        std::fs::write(fresh.index_path(), serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        assert!(fresh.save(&mut empty).is_err(), "an empty index was written over a new one");
    }

    /// `version` is written on every save, so it has to be read on every load.
    /// An index a newer build wrote parses as this one's shape - serde drops
    /// what it does not know - and the next `save` writes the file back without
    /// the dropped fields. Refusing is what makes the field mean anything, and
    /// it is what `Job::open` already does for the manifest.
    #[test]
    fn an_index_from_a_later_version_is_refused_rather_than_half_read() {
        let scratch = Scratch::new("index-version");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);

        let mut index = library.index().unwrap();
        index.version = INDEX_VERSION + 1;
        library.save(&mut index).unwrap();
        let on_disk = std::fs::read(library.index_path()).unwrap();

        assert!(matches!(library.index(), Err(LibraryError::Index { .. })));
        assert!(library.list_projects().is_err());
        // And nothing writes over it on the way past: a refused read is a
        // library left exactly as the newer build wrote it.
        assert!(library.rename_project(&project.id, "Renamed").is_err());
        assert!(library
            .create_project("New", StripMode::Single, None, None)
            .is_err());
        assert!(library.delete_project(&project.id).is_err());
        assert_eq!(
            std::fs::read(library.index_path()).unwrap(),
            on_disk,
            "the index was rewritten"
        );
    }

    /// Whatever a refused index costs the user, it is reported under a key the
    /// catalogue already has - inventing one is a `npm test` failure.
    #[test]
    fn a_refused_index_reports_a_key_the_catalogue_already_holds() {
        let error = LibraryError::Index {
            path: PathBuf::from("index.json"),
            detail: "version 2".into(),
        };
        assert_eq!(error.reason_key(), "notice.library.changeFailed");
    }

    #[test]
    fn a_project_survives_being_written_and_read_back() {
        let scratch = Scratch::new("round-trip");
        let library = library(&scratch);
        let created = library
            .create_project(
                "Nine Skies",
                StripMode::Longstrip,
                None,
                Some(ReadingDirection::Ltr),
            )
            .unwrap();

        let listed = library.list_projects().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
        assert_eq!(listed[0].name, "Nine Skies");
        assert_eq!(listed[0].mode, StripMode::Longstrip);
        assert_eq!(listed[0].reading_direction, ReadingDirection::Ltr);
        assert!(
            listed[0].chapters.is_empty(),
            "a new project invented a chapter"
        );
    }

    #[test]
    fn a_project_without_cloud_consent_reads_and_writes_none() {
        let old: IndexProject = serde_json::from_value(serde_json::json!({
            "id": "p1", "name": "P", "mode": "single", "source_path": null,
            "created": 0, "last_opened": 0,
        }))
        .unwrap();
        assert!(old.cloud_consent.is_none());
        assert!(serde_json::to_value(&old).unwrap().get("cloud_consent").is_none());
    }

    #[test]
    fn a_cloud_consent_covers_its_project_and_endpoint_only() {
        let scratch = Scratch::new("cloud-consent");
        let library = library(&scratch);
        let one = library.create_project("One", StripMode::Single, None, None).unwrap();
        let two = library.create_project("Two", StripMode::Single, None, None).unwrap();
        let a = library.create_chapter(&one.id, "A", None, None).unwrap().created().unwrap();
        let b = library.create_chapter(&one.id, "B", None, None).unwrap().created().unwrap();
        let c = library.create_chapter(&two.id, "C", None, None).unwrap().created().unwrap();
        let covers = |chapter: &str| library.cloud_consent_covers(chapter, CloudProvider::Modal, "mc-1", "fp");
        assert!(!covers(&a.id));

        library.record_cloud_consent(&a.id, CloudProvider::Modal, "mc-1", "fp", false);
        assert!(!covers(&a.id), "an answer without the statements does not stand");
        library.record_cloud_consent(&a.id, CloudProvider::Modal, "mc-1", "fp", true);
        assert!(covers(&a.id) && covers(&b.id), "the consent is the project's");
        assert!(!covers(&c.id), "another project asks for its own");
        assert!(!library.cloud_consent_covers(&a.id, CloudProvider::Modal, "mc-1", "moved"));
        assert!(!library.cloud_consent_covers(&a.id, CloudProvider::Beam, "mc-1", "fp"));
        assert!(!library.cloud_consent_covers(&a.id, CloudProvider::Modal, "mc-2", "fp"));
        let saved = std::fs::read_to_string(library.index_path()).unwrap();
        assert!(saved.contains("\"cloud_consent\""));

        assert!(library.set_cloud_consent(&b.id, None).unwrap());
        assert!(!covers(&a.id));
        assert!(!library.set_cloud_consent("nowhere", None).unwrap());
    }

    #[test]
    fn cloud_consents_are_withdrawn_per_endpoint_or_all_at_once() {
        let scratch = Scratch::new("cloud-consent-clear");
        let library = library(&scratch);
        let one = library.create_project("One", StripMode::Single, None, None).unwrap();
        let two = library.create_project("Two", StripMode::Single, None, None).unwrap();
        let three = library.create_project("Three", StripMode::Single, None, None).unwrap();
        let a = library.create_chapter(&one.id, "A", None, None).unwrap().created().unwrap();
        let b = library.create_chapter(&two.id, "B", None, None).unwrap().created().unwrap();
        let c = library.create_chapter(&three.id, "C", None, None).unwrap().created().unwrap();
        library.record_cloud_consent(&a.id, CloudProvider::Modal, "mc-1", "fp", true);
        library.record_cloud_consent(&b.id, CloudProvider::Modal, "mc-2", "fp", true);
        library.record_cloud_consent(&c.id, CloudProvider::Beam, "mc-1", "fp", true);
        let covers = |chapter: &str, provider, profile: &str| {
            library.cloud_consent_covers(chapter, provider, profile, "fp")
        };

        // Removing one endpoint withdraws only the consents given for it.
        assert_eq!(library.clear_cloud_consents(Some((CloudProvider::Modal, "mc-1"))).unwrap(), 1);
        assert!(!covers(&a.id, CloudProvider::Modal, "mc-1"));
        assert!(covers(&b.id, CloudProvider::Modal, "mc-2"));
        assert!(covers(&c.id, CloudProvider::Beam, "mc-1"), "the same id on another provider is another endpoint");
        assert_eq!(library.clear_cloud_consents(Some((CloudProvider::Modal, "mc-1"))).unwrap(), 0);

        // Turning cloud engines off withdraws every one, on disk too.
        assert_eq!(library.clear_cloud_consents(None).unwrap(), 2);
        assert!(!covers(&b.id, CloudProvider::Modal, "mc-2"));
        assert!(!covers(&c.id, CloudProvider::Beam, "mc-1"));
        let saved = std::fs::read_to_string(library.index_path()).unwrap();
        assert!(!saved.contains("\"cloud_consent\""));
        assert_eq!(library.clear_cloud_consents(None).unwrap(), 0);
    }

    #[test]
    fn a_cloud_consent_without_the_statements_does_not_stand() {
        let old: CloudConsent = serde_json::from_value(serde_json::json!({
            "provider": "modal", "profile_id": "mc-1", "origin_fingerprint": "fp", "granted_at": 1,
        }))
        .unwrap();
        assert!(!old.statements_accepted, "a consent from before the statements has none");
        let accepted = CloudConsent { statements_accepted: true, ..old.clone() };
        let json = serde_json::to_value(&accepted).unwrap();
        assert_eq!(json["statements_accepted"], true);
        assert_eq!(serde_json::from_value::<CloudConsent>(json).unwrap(), accepted);

        let scratch = Scratch::new("cloud-consent-old");
        let library = library(&scratch);
        let project = library.create_project("One", StripMode::Single, None, None).unwrap();
        let chapter = library.create_chapter(&project.id, "A", None, None).unwrap().created().unwrap();
        let covers = || library.cloud_consent_covers(&chapter.id, CloudProvider::Modal, "mc-1", "fp");
        assert!(library.set_cloud_consent(&chapter.id, Some(old)).unwrap());
        assert!(!covers(), "an old consent asks once more");
        library.record_cloud_consent(&chapter.id, CloudProvider::Modal, "mc-1", "fp", true);
        assert!(covers(), "then stands");
        let saved = std::fs::read_to_string(library.index_path()).unwrap();
        assert!(saved.contains("\"statements_accepted\": true") || saved.contains("\"statements_accepted\":true"));
    }

    #[test]
    fn reading_direction_defaults_to_right_to_left() {
        let scratch = Scratch::new("rtl");
        let library = library(&scratch);
        let project = library
            .create_project("Tsuki", StripMode::Single, None, None)
            .unwrap();
        assert_eq!(project.reading_direction, ReadingDirection::Rtl);
    }

    /// Numbered `max + 1`, unshifted.
    #[test]
    fn chapters_are_numbered_above_the_highest_and_a_gap_stays_a_gap() {
        let scratch = Scratch::new("numbering");
        let library = library(&scratch);
        let project = library
            .create_project("Emberfall", StripMode::Single, None, None)
            .unwrap();

        for expected in 1..=3 {
            let chapter = library
                .create_chapter(&project.id, &format!("Ch {expected}"), None, None)
                .unwrap()
                .created()
                .unwrap();
            assert_eq!(chapter.number, expected);
        }

        // The seam has no delete-chapter, so the gap is made by hand - the
        // property under test is that numbering reads the maximum rather than
        // the count.
        let mut index = library.index().unwrap();
        index.projects[0]
            .chapters
            .retain(|chapter| chapter.number != 2);
        library.save(&mut index).unwrap();

        let next = library
            .create_chapter(&project.id, "Ch 4", None, None)
            .unwrap()
            .created()
            .unwrap();
        assert_eq!(
            next.number, 4,
            "a removed chapter's number was handed out again"
        );
    }

    /// The number the New chapter dialog sends is the number the chapter gets,
    /// whatever the project already holds. Before this, both the dialog and
    /// this method derived `max + 1` independently, so a user who typed 12 into
    /// a project holding Ch. 1 got Ch. 2.
    #[test]
    fn a_given_number_is_stored_verbatim() {
        let scratch = Scratch::new("given-number");
        let library = library(&scratch);
        let project = library
            .create_project("Emberfall", StripMode::Single, None, None)
            .unwrap();

        let first = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        assert_eq!(first.number, 1);

        let jump = library
            .create_chapter(&project.id, "Ch 12", Some(12), Some(scratch.join("twelve")))
            .unwrap()
            .created()
            .unwrap();
        assert_eq!(jump.number, 12, "the number the caller gave was not kept");

        // Below the highest is a position too: numbering is not a counter.
        let back = library
            .create_chapter(&project.id, "Ch 5", Some(5), Some(scratch.join("five")))
            .unwrap()
            .created()
            .unwrap();
        assert_eq!(back.number, 5);

        // No number named still means `max + 1`, which is what the dialog
        // starts its field at.
        let next = library
            .create_chapter(&project.id, "Ch next", None, Some(scratch.join("next")))
            .unwrap()
            .created()
            .unwrap();
        assert_eq!(next.number, 13);
    }

    /// Deleting a chapter always takes the library's own record of it. The
    /// scans are the user's, and go only when asked for.
    #[test]
    fn deleting_a_chapter_takes_its_manifest_and_leaves_the_scans_alone() {
        let scratch = Scratch::new("delete-chapter");
        let library = library(&scratch);
        let scans = scans(&scratch.join("ch1"), 2);
        let project = library
            .create_project("Emberfall", StripMode::Single, None, None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, Some(scans.clone()))
            .unwrap()
            .created()
            .unwrap();

        let manifest = library.job_path(&project.id, &chapter.id);
        let sidecar = cleaner_core::project::sidecar_dir(&manifest);
        assert!(manifest.exists(), "the chapter was never written");

        let outcome = library
            .delete_chapter(&project.id, &chapter.id, false)
            .unwrap();
        assert!(matches!(
            outcome,
            ChapterDeleted::Deleted {
                number: 1,
                scans: Scans::Kept
            }
        ));

        assert!(!manifest.exists(), "the manifest outlived the chapter");
        assert!(
            !sidecar.exists(),
            "the sidecar directory outlived the chapter"
        );
        assert!(
            scans.exists(),
            "the scans were removed without being asked for"
        );
        assert!(library.index().unwrap().projects[0].chapters.is_empty());
    }

    /// A folder of JPEGs opens as a chapter instead of being refused page by
    /// page, and every answer about *where the chapter came from* still names
    /// the user's folder rather than the library directory the copies went into.
    #[test]
    fn jpeg_import_estimate_bounds_verbatim_source() {
        let scratch = Scratch::new("jpeg-estimate");
        let raws = jpeg_scans(&scratch.join("raws"), 1);
        let path = raws.join("001.jpg");
        let source = std::fs::read(&path).unwrap();
        let estimate = estimate_import_bytes(&[path]);

        assert_eq!(estimate,source.len() as u64 + 1_048_576,
            "native JPEG needs its exact bytes plus the chapter reserve");
    }

    #[test]
    fn converted_bmp_estimate_includes_the_owned_archived_original() {
        let scratch=Scratch::new("bmp-archive-estimate");
        let (width,height)=(1024u32,1024u32);
        let pixels=(width*height*3) as usize;
        let mut source=vec![255u8;54+pixels];source[..54].fill(0);source[..2].copy_from_slice(b"BM");
        let length=source.len() as u32;source[2..6].copy_from_slice(&length.to_le_bytes());
        source[10..14].copy_from_slice(&54u32.to_le_bytes());source[14..18].copy_from_slice(&40u32.to_le_bytes());
        source[18..22].copy_from_slice(&width.to_le_bytes());source[22..26].copy_from_slice(&height.to_le_bytes());
        source[26..28].copy_from_slice(&1u16.to_le_bytes());source[28..30].copy_from_slice(&24u16.to_le_bytes());
        let path=scratch.join("page.bmp");std::fs::write(&path,&source).unwrap();
        let raster=cleaner_core::image::foreign::decode(&source).unwrap();
        let working=encode(&raster,Format::Png).unwrap();assert!(source.len()>working.len()*10);
        assert_eq!(estimate_import_bytes(&[path]),source.len() as u64+working.len() as u64+2*1_048_576);
    }

    #[test]
    fn cmyk_import_estimate_uses_tiff_output_size() {
        let raster = fixtures::by_name("cmyk8").raster;
        let tiff = encode(&raster, Format::Tiff).unwrap();
        assert_eq!(
            estimate_converted_bytes(&raster),
            Some(tiff.len() as u64 + 1_048_576)
        );
    }

    #[test]
    fn a_folder_of_jpegs_is_imported_verbatim_into_the_chapter() {
        let scratch = Scratch::new("convert-chapter");
        let library = library(&scratch);
        let raws = jpeg_scans(&scratch.join("raws"), 2);
        let project = library
            .create_project("Neon Alley", StripMode::Single, None, None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, Some(raws.clone()))
            .unwrap()
            .created()
            .unwrap();

        assert_eq!(
            chapter.pages.len(),
            2,
            "the JPEGs were refused: {:?}",
            chapter.input_reports
        );
        assert_eq!(chapter.source_path, raws.to_string_lossy());

        let manifest_path = library.job_path(&project.id, &chapter.id);
        let converted = cleaner_core::project::sidecar_dir(&manifest_path).join(PAGES_DIR);
        assert_eq!(std::fs::read_dir(&converted).unwrap().count(), 2);

        let manifest = library.read_manifest(&manifest_path).unwrap();
        for source in &manifest.sources {
            let copied = manifest_path.parent().unwrap().join(&source.rel_path);
            assert_eq!(copied.extension().unwrap(), "jpg");
            assert!(copied.starts_with(&converted));
            let original = source
                .converted_from
                .as_ref()
                .expect("the JPEG it came from");
            assert_eq!(std::fs::read(&copied).unwrap(), std::fs::read(manifest_path.parent().unwrap().join(original)).unwrap());
        }

        // The originals are still where the user put them, all of them.
        assert_eq!(std::fs::read_dir(&raws).unwrap().count(), 2);

        assert!(!chapter.input_reports.iter().any(|notice| notice.key == "notice.input.converted"));
        // The chapter remains readable independently of the user's scan folder.
        std::fs::remove_dir_all(&raws).unwrap();
        let job = Job::open(&manifest_path).unwrap();
        for index in 0..2 {
            let bytes = std::fs::read(job.source_path(index).unwrap()).unwrap();
            let source = cleaner_core::image::decode(&bytes).unwrap();
            for variant in [crate::tile::Variant::Source, crate::tile::Variant::Cleaned] {
                let preview = crate::tile::render(&manifest_path, index, variant, None).unwrap();
                let decoded = cleaner_core::image::decode(&preview).unwrap();
                let expected=cleaner_core::image::proxy::display(&source).unwrap();
                assert_eq!(decoded.data, expected.data);
                assert_eq!(decoded.icc, expected.icc);
            }
        }
        assert_eq!(std::fs::read_dir(&converted).unwrap().count(), 2, "preview created a permanent PNG");
    }

    /// The import, end to end: a folder of PNGs needs no conversion and is
    /// taken into the library anyway, so deleting the scan folder afterwards
    /// leaves the chapter whole.
    #[test]
    fn a_chapter_keeps_its_pages_after_the_scan_folder_is_deleted() {
        let scratch = Scratch::new("import-chapter");
        let library = library(&scratch);
        let raws = scans(&scratch.join("raws"), 2);
        let project = library
            .create_project("Wandering Moon", StripMode::Single, None, None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, Some(raws.clone()))
            .unwrap()
            .created()
            .unwrap();
        assert_eq!(
            chapter.pages.len(),
            2,
            "the PNGs were refused: {:?}",
            chapter.input_reports
        );

        let manifest_path = library.job_path(&project.id, &chapter.id);
        let pages = cleaner_core::project::sidecar_dir(&manifest_path).join(PAGES_DIR);
        assert_eq!(std::fs::read_dir(&pages).unwrap().count(), 2);

        // Nothing was converted, so the chapter must not say anything was.
        assert!(
            !chapter
                .input_reports
                .iter()
                .any(|notice| notice.key == "notice.input.converted"),
            "a copy was reported as a conversion"
        );

        // While the folder is still there, "output never overwrites input"
        // answers about it - from `converted_from`, since no page's own file is
        // in it any more.
        let job = Job::open(&manifest_path).expect("the job opens");
        assert!(job.output_refusal(&raws).is_some());
        assert_eq!(
            job.origin_path(0).unwrap().parent().unwrap().file_name(),
            raws.file_name()
        );
        drop(job);

        // The user's folder goes, and with it every path the manifest used to
        // depend on.
        std::fs::remove_dir_all(&raws).unwrap();

        let manifest = library.read_manifest(&manifest_path).unwrap();
        let job = Job::open(&manifest_path).expect("the job still opens");
        for index in 0..manifest.sources.len() {
            let page = job.source_path(index).expect("a page path");
            assert!(
                page.starts_with(&pages),
                "{page:?} is not the library's own file"
            );
            assert!(page.exists(), "{page:?} did not survive the scan folder");
        }
        // Every source verifies against the library's own copy, so a resume
        // finds nothing stale and drops no patches.
        assert!(job
            .verify_sources()
            .iter()
            .all(|state| *state == cleaner_core::project::SourceState::Unchanged));

        // And the origin is still the folder the user chose, so the export's
        // default destination is still theirs and not one inside the library  -
        // a directory the exporter creates, rather than a path it cannot form.
        // (Lexically, since `rel_path` climbs out of the library with `..` and
        // neither end canonicalises once the folder is gone.)
        let default_dir = job.origin_path(0).unwrap().parent().unwrap().to_path_buf();
        assert_eq!(default_dir.file_name(), raws.file_name());
        assert_ne!(default_dir, pages);
    }

    /// The whole reason `Job::origin_path` exists: an export of a converted
    /// chapter must land beside the user's scans, not inside the library.
    #[test]
    fn a_converted_chapters_default_output_is_a_sibling_of_the_scans() {
        let scratch = Scratch::new("convert-export-dir");
        let library = library(&scratch);
        let raws = jpeg_scans(&scratch.join("raws"), 1);
        let project = library
            .create_project("Neon Alley", StripMode::Single, None, None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, Some(raws.clone()))
            .unwrap()
            .created()
            .unwrap();

        let job = Job::open(&library.job_path(&project.id, &chapter.id)).unwrap();
        // Canonicalised, because the manifest holds a relative path and the
        // join back out of the library is a `../..` chain - true, and not a
        // path to compare textually.
        let origin = job.origin_path(0).unwrap().canonicalize().unwrap();
        assert_eq!(origin.parent().unwrap(), raws.canonicalize().unwrap(),);
        // And "output never overwrites input" covers the JPEG too.
        assert!(job.output_refusal(&raws).is_some());
    }

    #[test]
    fn deleting_a_chapter_with_its_scans_removes_the_folder() {
        let scratch = Scratch::new("delete-chapter-scans");
        let library = library(&scratch);
        let scans = scans(&scratch.join("ch1"), 2);
        let project = library
            .create_project("Emberfall", StripMode::Single, None, None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, Some(scans.clone()))
            .unwrap()
            .created()
            .unwrap();

        let outcome = library
            .delete_chapter(&project.id, &chapter.id, true)
            .unwrap();
        assert!(matches!(
            outcome,
            ChapterDeleted::Deleted {
                scans: Scans::Removed,
                ..
            }
        ));
        assert!(
            !scans.exists(),
            "the scans were kept after the user asked for them to go"
        );
    }

    /// The one refusal: a chapter reading the project's own folder is every
    /// other chapter's folder too, so deleting one chapter must not empty it.
    #[test]
    fn a_chapters_scans_are_kept_when_they_are_the_projects_own_folder() {
        let scratch = Scratch::new("delete-chapter-own-folder");
        let library = library(&scratch);
        let root = scans(&scratch.join("root"), 2);
        let project = library
            .create_project("Emberfall", StripMode::Single, Some(root.clone()), None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let outcome = library
            .delete_chapter(&project.id, &chapter.id, true)
            .unwrap();
        assert!(matches!(
            outcome,
            ChapterDeleted::Deleted {
                scans: Scans::KeptProjectFolder,
                ..
            }
        ));
        assert!(root.exists(), "deleting one chapter emptied the project");
    }

    #[test]
    fn deleting_a_chapter_that_is_not_there_is_no_chapter_rather_than_an_error() {
        let scratch = Scratch::new("delete-chapter-missing");
        let library = library(&scratch);
        let project = library
            .create_project("Emberfall", StripMode::Single, None, None)
            .unwrap();
        assert!(matches!(
            library.delete_chapter(&project.id, "c404", false).unwrap(),
            ChapterDeleted::NoChapter
        ));
        assert!(matches!(
            library.delete_chapter("p404", "c1", false).unwrap(),
            ChapterDeleted::NoChapter
        ));
    }

    #[test]
    fn a_chapter_of_an_unknown_project_is_none_rather_than_an_error() {
        let scratch = Scratch::new("unknown-project");
        let library = library(&scratch);
        assert!(matches!(
            library.create_chapter("p404", "Ch 1", None, None).unwrap(),
            NewChapter::NoProject
        ));
        assert!(library.rename_project("p404", "x").unwrap().is_none());
        assert!(!library.delete_project("p404").unwrap());
        assert!(library
            .open_chapter("p404", "c404", false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn renaming_changes_the_label_and_nothing_else() {
        let scratch = Scratch::new("rename");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 2);
        let renamed = library
            .rename_project(&project.id, "Wandering Moon (v2)")
            .unwrap()
            .unwrap();
        assert_eq!(renamed.name, "Wandering Moon (v2)");
        assert_eq!(renamed.source_path, project.source_path);
        assert_eq!(renamed.mode, project.mode);
    }

    /// The copy promises it: *"The pages in {path} are left exactly as they
    /// are."* And the job's own record is left alone too, because removing a
    /// project is one click.
    #[test]
    fn deleting_a_project_touches_no_file_on_disk() {
        let scratch = Scratch::new("delete");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 3);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        let job = library.job_path(&project.id, &chapter.id);
        assert!(job.exists());

        assert!(library.delete_project(&project.id).unwrap());
        assert!(library.list_projects().unwrap().is_empty());
        assert!(
            job.exists(),
            "the job manifest was deleted with the library entry"
        );
        assert_eq!(std::fs::read_dir(scratch.join("raws")).unwrap().count(), 3);
    }

    /* ---------- the tree ---------- */

    #[test]
    fn a_chapters_pages_come_from_the_projects_source_folder() {
        let scratch = Scratch::new("pages");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 4);
        library
            .create_chapter(&project.id, "Vault of Ash", None, None)
            .unwrap()
            .created();

        let listed = library.list_projects().unwrap();
        let chapter = &listed[0].chapters[0];
        assert_eq!(chapter.pages.len(), 4);
        assert_eq!(chapter.source_format, "PNG");
        assert_eq!(chapter.pages[0].file, "001.png");
        assert_eq!(chapter.pages[0].index, 0);
        assert_eq!(chapter.pages[0].number, 1);
        assert_eq!(chapter.pages[3].file, "004.png");
        assert!(chapter.pages.iter().all(|page| page.status == "unclean"));
        assert!(chapter
            .pages
            .iter()
            .all(|page| page.width > 0 && page.height > 0));
    }

    #[test]
    fn a_subfolder_named_after_the_chapter_is_the_chapter() {
        let scratch = Scratch::new("subfolder");
        let library = library(&scratch);
        let root = scratch.join("raws");
        scans(&root, 2);
        scans(&root.join("Ch 12"), 5);
        let project = library
            .create_project("WM", StripMode::Single, Some(root), None)
            .unwrap();

        library
            .create_chapter(&project.id, "Ch 12", None, None)
            .unwrap()
            .created();
        let listed = library.list_projects().unwrap();
        assert_eq!(
            listed[0].chapters[0].pages.len(),
            5,
            "the subfolder was not preferred"
        );
    }

    /* ---------- where a chapter's pages come from ---------- */

    /// **Acceptance test.** Two
    /// chapters, two folders, two different sets of files - which is what the
    /// seam could not express while `createChapter` took a project id and a name
    /// and nothing else.
    #[test]
    fn two_chapters_given_two_folders_hold_two_different_sets_of_files() {
        let scratch = Scratch::new("two-sources");
        let library = library(&scratch);
        // Distinct bytes per folder as well as per page: two folders of the
        // same three pages would be deduplicated against each other by nothing
        // - ingest is per chapter - and the test would pass without proving it.
        let east = tagged_scans(&scratch.join("east"), 3, 0x10);
        let west = tagged_scans(&scratch.join("west"), 5, 0x90);
        let project = library
            .create_project("Two Coasts", StripMode::Single, Some(east.clone()), None)
            .unwrap();

        let first = library
            .create_chapter(&project.id, "East", None, Some(east))
            .unwrap()
            .created()
            .unwrap();
        let second = library
            .create_chapter(&project.id, "West", None, Some(west))
            .unwrap()
            .created()
            .unwrap();

        assert_eq!(first.pages.len(), 3);
        assert_eq!(second.pages.len(), 5);
        let shas = |chapter: &ApiChapter| -> Vec<String> {
            chapter
                .pages
                .iter()
                .map(|page| page.source_sha.clone())
                .collect()
        };
        assert!(
            shas(&first).iter().all(|sha| !shas(&second).contains(sha)),
            "the two chapters ingested the same files"
        );
        assert!(first.source_path.ends_with("east"), "{}", first.source_path);
        assert!(
            second.source_path.ends_with("west"),
            "{}",
            second.source_path
        );
    }

    /// The same thing over the repository's own scans rather than over
    /// synthesised ones, because the fixtures are real PNGs at real page sizes
    /// and the failure described is a failure about files on a disk.
    ///
    /// One project, one scan folder, two chapters laid out the way a scanlator
    /// actually lays one out - a subfolder each - and the two chapters come back
    /// holding the files that are in their own folder and no others.
    #[test]
    fn two_chapters_of_one_scan_folder_hold_the_files_that_are_in_their_own_subfolder() {
        let pages = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/pages");
        let scratch = Scratch::new("real-scans");
        let library = library(&scratch);
        let root = scratch.join("Tsuki to Hane");

        let copy = |into: &str, names: &[&str]| -> Vec<String> {
            let dir = root.join(into);
            std::fs::create_dir_all(&dir).unwrap();
            for name in names {
                std::fs::copy(pages.join(name), dir.join(name)).unwrap();
            }
            names.iter().map(|name| (*name).to_owned()).collect()
        };
        let in_one = copy(
            "Ch. 1",
            &["page-dialogue.png", "page-sfx.png", "page-screentone.png"],
        );
        let in_two = copy("Ch. 2", &["strip-01.png", "strip-02.png"]);

        let project = library
            .create_project("Tsuki to Hane", StripMode::Single, Some(root.clone()), None)
            .unwrap();
        // No path given: the subfolder named after the chapter is the chapter,
        // which is the inference - and it is distinct per chapter, so the guard
        // never fires and neither chapter borrows the other's files.
        let one = library
            .create_chapter(&project.id, "Ch. 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        let two = library
            .create_chapter(&project.id, "Ch. 2", None, None)
            .unwrap()
            .created()
            .unwrap();

        let files = |chapter: &ApiChapter| -> Vec<String> {
            let mut names: Vec<String> = chapter.pages.iter().map(|p| p.file.clone()).collect();
            names.sort();
            names
        };
        let sorted = |mut names: Vec<String>| {
            names.sort();
            names
        };
        assert_eq!(files(&one), sorted(in_one));
        assert_eq!(files(&two), sorted(in_two));
        assert!(one.source_path.ends_with("Ch. 1"), "{}", one.source_path);
        assert!(two.source_path.ends_with("Ch. 2"), "{}", two.source_path);
    }

    /// The other half: the fallback survives, because a project that
    /// points at a folder of scans should not make anyone re-state it for the
    /// first chapter - but it can no longer hand that folder to a second.
    #[test]
    fn a_second_chapter_with_nothing_to_tell_it_apart_is_refused_rather_than_given_the_first_ones_files(
    ) {
        let scratch = Scratch::new("fallback-guard");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 4);

        let first = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        assert_eq!(first.pages.len(), 4);

        match library
            .create_chapter(&project.id, "Ch 2", None, None)
            .unwrap()
        {
            NewChapter::SourceTaken { chapter } => assert_eq!(chapter, "Ch 1"),
            other => panic!("the project folder was handed out twice: {other:?}"),
        }
        // Refused means refused: no half-made chapter, and no number spent.
        assert_eq!(library.list_projects().unwrap()[0].chapters.len(), 1);

        // And naming the folder is what unblocks it - an explicit path is never
        // inferred, so it is never refused for having been inferred.
        let elsewhere = scans(&scratch.join("ch2"), 2);
        let second = library
            .create_chapter(&project.id, "Ch 2", None, Some(elsewhere))
            .unwrap()
            .created()
            .unwrap();
        assert_eq!(second.pages.len(), 2);
        assert_eq!(second.number, 2);
    }

    /// A chapter whose index row predates the source path - the shape every
    /// existing library holds. Nothing is migrated and nothing is re-pointed:
    /// the manifest still names the files it always named, and the folder is
    /// *read back out of it* rather than guessed at.
    #[test]
    fn a_chapter_from_before_the_seam_carried_a_path_keeps_its_files_and_still_says_where_they_are()
    {
        let scratch = Scratch::new("legacy");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 3);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        // Roll the row back to what a build before this one wrote.
        let mut index = library.index().unwrap();
        index.projects[0].chapters[0].source_path = None;
        library.save(&mut index).unwrap();

        let listed = library.list_projects().unwrap();
        let legacy = &listed[0].chapters[0];
        assert_eq!(
            legacy.pages.len(),
            3,
            "an unrecorded source lost the chapter its pages"
        );
        assert_eq!(legacy.id, chapter.id);
        assert!(
            legacy.source_path.ends_with("raws"),
            "{}",
            legacy.source_path
        );

        // And the guard sees it, so the folder it reads is not handed to a
        // second chapter just because the row does not name it.
        assert!(matches!(
            library
                .create_chapter(&project.id, "Ch 2", None, None)
                .unwrap(),
            NewChapter::SourceTaken { .. }
        ));
    }

    /// A chapter is not dropped and the read does not fail, because a chapter
    /// leaves the library only when a user removes the project - and because
    /// `max + 1` would otherwise hand its number out again.
    #[test]
    fn a_chapter_whose_job_is_unreadable_is_listed_with_no_pages() {
        let scratch = Scratch::new("broken-job");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 3);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        std::fs::write(library.job_path(&project.id, &chapter.id), b"{ not json").unwrap();

        let listed = library.list_projects().unwrap();
        assert_eq!(listed[0].chapters.len(), 1);
        assert!(listed[0].chapters[0].pages.is_empty());
        // Its own folder, because the property under test is the numbering and
        // the first chapter already reads the project's own.
        let next = scans(&scratch.join("ch2"), 1);
        assert_eq!(
            library
                .create_chapter(&project.id, "Ch 2", None, Some(next))
                .unwrap()
                .created()
                .unwrap()
                .number,
            2,
            "an unreadable job took its chapter out of the numbering"
        );
    }

    #[test]
    fn a_chapter_with_no_source_folder_has_no_job_and_still_lists() {
        let scratch = Scratch::new("no-source");
        let library = library(&scratch);
        let project = library
            .create_project("Sketches", StripMode::Single, None, None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        assert!(chapter.pages.is_empty());
        assert!(matches!(
            library.resolve_chapter(&chapter.id),
            Err(LibraryError::NoJob { .. })
        ));
    }

    #[test]
    fn resolve_chapter_finds_the_job_across_projects() {
        let scratch = Scratch::new("resolve");
        let library = library(&scratch);
        let first = a_project(&library, &scratch, 2);
        let second = library
            .create_project(
                "Second",
                StripMode::Single,
                Some(scratch.join("raws")),
                None,
            )
            .unwrap();
        let a = library
            .create_chapter(&first.id, "A", None, None)
            .unwrap()
            .created()
            .unwrap();
        let b = library
            .create_chapter(&second.id, "B", None, None)
            .unwrap()
            .created()
            .unwrap();

        assert_ne!(a.id, b.id, "two chapters shared an id");
        assert_eq!(
            library.resolve_chapter(&a.id).unwrap(),
            library.job_path(&first.id, &a.id)
        );
        assert_eq!(
            library.resolve_chapter(&b.id).unwrap(),
            library.job_path(&second.id, &b.id)
        );
        assert!(matches!(
            library.resolve_chapter("c404"),
            Err(LibraryError::Unknown { .. })
        ));
    }

    /* ---------- regions, out of the manifest ---------- */

    #[test]
    fn a_recorded_patch_becomes_a_cleaned_region_with_its_mask() {
        let scratch = Scratch::new("regions");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 2);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        let page_width = job.project.sources[0].w;
        job.complete_region(
            0,
            &a_patch("r1", Rect::new(0, 0, page_width / 4, 8), Engine::Fill),
            None,
        )
        .unwrap();
        job.leave_untouched(
            0,
            Rect::new(4, 6, 10, 10),
            "review.reason.gateSkippedNotJapanese",
        )
        .unwrap();
        job.leave_untouched(1, Rect::new(2, 2, 8, 8), "decline.reason.qualityMetric")
            .unwrap();

        // A listing carries headers only, and a header still answers the
        // two questions Home asks of it.
        let listed = library.list_projects().unwrap();
        let headers = &listed[0].chapters[0].pages;
        assert_eq!(headers[0].status, "cleaned");
        assert_eq!(
            headers[1].status, "unclean",
            "an untouched region cleaned a page"
        );
        assert!(headers
            .iter()
            .all(|page| !page.resident && page.regions.is_empty()));
        assert_eq!(
            headers[0].region_count, 2,
            "a header counts what it did not build"
        );

        let pages = library.load_pages(&chapter.id, &[0, 1]).unwrap();
        assert!(pages.iter().all(|page| page.resident));
        let cleaned = &pages[0].regions[0];
        assert_eq!(cleaned.outcome, "cleaned");
        assert_eq!(cleaned.source, "auto");
        let mask = cleaned.mask.as_ref().expect("a cleaned region has a mask");
        assert_eq!(mask.id, format!("{}-m1", cleaned.id));
        assert_eq!(mask.region_id, cleaned.id);
        assert_eq!(mask.fill_mode, "match-surround");
        assert_eq!(mask.provenance.created, "2025-10-09T08:53:20Z");

        // The bbox crosses the seam as a percentage of the page, not as pixels.
        assert!(
            (cleaned.bbox.w - 25.0).abs() < 1e-9,
            "bbox {:?} is not in percent",
            cleaned.bbox
        );

        let gated = &pages[0].regions[1];
        assert_eq!(gated.outcome, "gate-skipped");
        assert_eq!(gated.gate_skip_cause, Some("not-japanese"));
        assert!(gated.mask.is_none());

        let declined = &pages[1].regions[0];
        assert_eq!(declined.outcome, "declined");
        assert_eq!(
            declined.decline_reason.as_deref(),
            Some("decline.reason.qualityMetric")
        );
    }

    /// The Layers panel's delete, and its undo, end to end: the mask leaves the
    /// page and the manifest, the page stops being a cleaned page, and putting
    /// it back restores both. Under the seam's own vocabulary - a mask id in,
    /// `{region, pageStatus}` out - because that is what the interface sends.
    fn a_stored_detection(job: &mut Job, page: &str, bbox: Rect) -> String {
        a_stored_detection_at(job, page, bbox, false)
    }

    /// [`a_stored_detection`], inside a speech bubble or not.
    fn a_stored_detection_at(job: &mut Job, page: &str, bbox: Rect, inside: bool) -> String {
        let id = job.next_detection_id(page);
        job.store_detection(cleaner_core::project::DetectedRegion {
            id: id.clone(),
            source_idx: 0,
            bbox,
            mask_ref: String::new(),
            inside,
            balloon_color: None,
            script: Some("ko".into()),
            pick: "lama".into(),
            detector: "cloud".into(),
            created: 1_760_000_000,
            mask_sha256: String::new(),
            order: 0,
            review_state: Some("review.reason.unusuallyLarge".into()),
            fit: None, group: None,
            padding_from_seed: false,
            padding_px: 0,
        }, &cleaner_core::mask::Mask::filled(bbox), &cleaner_core::mask::Mask::filled(bbox)).unwrap();
        id
    }

    /// A detection's row says whether its text is inside a speech bubble, which
    /// picks the colour the interface draws its mask in. A cleaned layer says
    /// nothing, and the seam spells the field `insideBubble`.
    #[test]
    fn a_detection_row_carries_its_balloon_answer() {
        let scratch = Scratch::new("detected-inside");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let page = page_id(&chapter.id, 0);
        let mut job = Job::open(&path).unwrap();
        job.complete_region(0, &a_patch(&format!("{page}-r0"), Rect::new(0, 0, 12, 8), Engine::Fill), None)
            .unwrap();
        let inside = a_stored_detection_at(&mut job, &page, Rect::new(20, 10, 8, 6), true);
        let outside = a_stored_detection_at(&mut job, &page, Rect::new(30, 20, 8, 6), false);
        invalidate_manifest_cache(&path);

        let pages = library.load_pages(&chapter.id, &[0]).unwrap();
        let row = |id: &str| pages[0].regions.iter().find(|region| region.id == id).unwrap();
        assert_eq!(row(&inside).inside_bubble, Some(true));
        assert_eq!(row(&outside).inside_bubble, Some(false));
        assert_eq!(row(&format!("{page}-r0")).inside_bubble, None, "a cleaned layer has no balloon answer");
        let json = serde_json::to_value(row(&inside)).unwrap();
        assert_eq!(json["insideBubble"], true);
        assert_eq!(json["paddingPx"], 0);
        assert_eq!(row(&outside).padding_px, Some(0));
        assert!(serde_json::to_value(row(&format!("{page}-r0"))).unwrap().get("paddingPx").is_none());
        assert_eq!(serde_json::to_value(row(&outside)).unwrap()["insideBubble"], false);
    }

    /// The text type flip on one record: the balloon's measured colour goes,
    /// and the pick moves only when it paints flat paper outside a balloon.
    #[test]
    fn retyping_a_detection_clears_its_balloon_colour_and_moves_only_a_contradicting_pick() {
        let record = |inside: bool, pick: &str| DetectedRegion {
            id: "c-p001-d1".into(),
            source_idx: 0,
            bbox: Rect::new(1, 2, 3, 4),
            mask_ref: "detections/c-p001-d1.mask".into(),
            inside,
            balloon_color: inside.then_some([236; 3]),
            script: Some("ja".into()),
            pick: pick.into(),
            detector: "local".into(),
            created: 1,
            mask_sha256: "digest".into(),
            order: 3,
            review_state: None,
            fit: None,
            group: None,
            padding_from_seed: false,
            padding_px: 0,
        };
        for (inside, pick, wanted) in [
            (true, "fill", "lama"),
            (true, "solid", "lama"),
            (true, "lama", "lama"),
            (false, "lama", "lama"),
            (false, "fill", "fill"),
            (false, "solid", "solid"),
        ] {
            let mut row = record(inside, pick);
            assert!(retype_detection(&mut row, !inside), "{pick} moved off inside={inside}");
            assert_eq!(row.inside, !inside);
            assert_eq!(row.balloon_color, None);
            assert_eq!(row.pick, wanted, "{pick} moved off inside={inside}");
            let untouched = DetectedRegion { inside: row.inside, balloon_color: None, pick: row.pick.clone(), ..record(inside, pick) };
            assert_eq!(row, untouched, "nothing else about the detection changes");
        }
        for inside in [true, false] {
            let mut row = record(inside, "fill");
            assert!(!retype_detection(&mut row, inside), "the type it already has");
            assert_eq!(row, record(inside, "fill"));
        }
    }

    /// `setDetectionType` end to end: the manifest's record flips under the
    /// chapter's lock, the answer is the row the interface draws (its
    /// `insideBubble` and the rung it starts on), the page stays `detected`,
    /// the masks are not touched, and asking for the type it has writes
    /// nothing. An id that is not a stored detection answers `None`, which
    /// the command rejects as `not_a_detection`.
    #[test]
    fn setting_a_detection_type_flips_the_stored_record_and_refuses_a_non_detection() {
        let scratch = Scratch::new("detected-retype");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let page = page_id(&chapter.id, 0);
        let mut job = Job::open(&path).unwrap();
        let cleaned = format!("{page}-r0");
        job.complete_region(0, &a_patch(&cleaned, Rect::new(0, 0, 12, 8), Engine::Fill), None)
            .unwrap();
        let id = a_stored_detection_at(&mut job, &page, Rect::new(20, 10, 8, 6), true);
        let row = job.project.detections.iter_mut().find(|row| row.id == id).unwrap();
        row.balloon_color = Some([240; 3]);
        row.pick = "fill".into();
        job.flush().unwrap();
        let mask_sha = job.project.detections[0].mask_sha256.clone();
        invalidate_manifest_cache(&path);

        let region = library.set_detection_inside(&id, false).unwrap().expect("a stored detection");
        assert_eq!((region.id.as_str(), region.outcome, region.inside_bubble), (id.as_str(), "detected", Some(false)));
        let mask = region.mask.as_ref().unwrap();
        assert_eq!(mask.provenance.engine, Engine::Lama, "fill paints flat paper, which only a balloon promises");
        assert_eq!(mask.provenance.params_snapshot["balloon_color"], serde_json::Value::Null);
        let job = Job::open(&path).unwrap();
        let stored = &job.project.detections[0];
        assert_eq!((stored.inside, stored.balloon_color, stored.pick.as_str()), (false, None, "lama"));
        assert_eq!(stored.mask_sha256, mask_sha, "the mask is not touched");
        assert!(job.sidecar().join(&stored.mask_ref).exists());
        assert_eq!(library.load_pages(&chapter.id, &[0]).unwrap()[0].status, "detected");

        let before = std::fs::read(&path).unwrap();
        let same = library.set_detection_inside(&id, false).unwrap().unwrap();
        assert_eq!(same.inside_bubble, Some(false));
        assert_eq!(std::fs::read(&path).unwrap(), before, "the type it has already writes nothing");

        let back = library.set_detection_inside(&id, true).unwrap().unwrap();
        assert_eq!(back.inside_bubble, Some(true));
        let stored = Job::open(&path).unwrap().project.detections[0].clone();
        assert_eq!((stored.inside, stored.balloon_color, stored.pick.as_str()), (true, None, "lama"),
            "every rung cleans inside a balloon, so the pick stays");

        assert!(library.set_detection_inside(&cleaned, true).unwrap().is_none(), "a cleaned layer has no type to set");
        assert!(library.set_detection_inside(&format!("{page}-d99"), true).unwrap().is_none());
        let job = Job::open(&path).unwrap();
        assert_eq!(job.project.patches.len(), 1);
        assert_eq!(job.project.patches[0].id, cleaned);
    }

    /// A stored detection is a region of its page with its mask, counted and
    /// not done, and it makes the page `detected`. Deleting it - through the
    /// mask, or through a restore that says it was not there - removes the
    /// record and both its files.
    #[test]
    fn a_detection_is_a_detected_region_and_deleting_it_removes_it() {
        let scratch = Scratch::new("detected-region");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 2);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let page = page_id(&chapter.id, 0);
        let mut job = Job::open(&path).unwrap();
        job.complete_region(0, &a_patch(&format!("{page}-r0"), Rect::new(0, 0, 12, 8), Engine::Fill), None)
            .unwrap();
        let first = a_stored_detection(&mut job, &page, Rect::new(20, 10, 8, 6));
        let second = a_stored_detection(&mut job, &page, Rect::new(30, 20, 8, 6));
        invalidate_manifest_cache(&path);

        let pages = library.load_pages(&chapter.id, &[0, 1]).unwrap();
        assert_eq!(pages[0].status, "detected");
        assert_eq!((pages[0].region_count, pages[0].done_count, pages[0].review_count), (3, 1, 0));
        assert_eq!(pages[1].status, "unclean");
        let detected = pages[0].regions.iter().find(|region| region.id == first).unwrap();
        assert_eq!((detected.outcome, detected.source, detected.detected), ("detected", "auto", true));
        assert!(!detected.unusually_large, "a size warning is the patch's, once cleaned");
        let mask = detected.mask.as_ref().expect("a detection carries its mask");
        assert_eq!(mask.id, format!("{first}-m1"));
        assert_eq!(mask.fill_mode, "reconstruct");
        assert_eq!(mask.provenance.engine, Engine::Lama);
        assert_eq!(mask.provenance.execution_provider, "cloud");
        assert_eq!(mask.provenance.params_snapshot["detected"], true);
        assert_eq!(mask.provenance.params_snapshot["pick"], "lama");
        assert!(!mask.provenance.mask_sha256.is_empty());
        assert!(!library.list_projects().unwrap()[0].chapters[0].no_text_detected);

        // `deleteMask` on the detection's mask id.
        let (_, status) = library.remove_detection(region_of_mask(&format!("{first}-m1"))).unwrap().unwrap();
        assert_eq!(status, "detected", "the second detection still waits");
        let job = Job::open(&path).unwrap();
        assert!(!job.sidecar().join(format!("detections/{first}.mask")).exists());
        assert!(!job.sidecar().join(format!("detections/{first}.ink")).exists());
        assert!(library.remove_detection(&first).unwrap().is_none(), "gone is gone");
        assert!(library.set_mask_visible(&second, false).unwrap().is_none(),
            "a detection has no visible flag to turn off");

        // `restoreRegion(id, null)` - the redo of a delete.
        let (_, status) = library.remove_region(&second).unwrap().unwrap();
        assert_eq!(status, "cleaned");
        let job = Job::open(&path).unwrap();
        assert!(job.project.detections.is_empty());
        assert!(!job.sidecar().join(format!("detections/{second}.mask")).exists());
        assert_eq!(job.project.patches.len(), 1, "the patch beside them is untouched");
    }

    /// A region the cloud attempt journal flags carries the reason on a
    /// detection and on a layer alike, is counted in review and indexed for
    /// navigation. Two attempts on one region keep two entries: settling one
    /// leaves the other's flag. An attempt a journal read could not judge
    /// keeps what it flagged before; the rest are replaced. The most urgent
    /// reason wins, and only a committed result gone missing is `repairNeeded`.
    /// (The only test that writes the process-wide set, so parallel tests
    /// cannot race on it.)
    #[test]
    fn cloud_attention_flags_a_region_until_every_attempt_on_it_is_settled() {
        let scratch = Scratch::new("repair-needed");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let page = page_id(&chapter.id, 0);
        let layer = format!("{page}-r0");
        let plain = format!("{page}-r1");
        let mut job = Job::open(&path).unwrap();
        job.complete_region(0, &a_patch(&layer, Rect::new(0, 0, 12, 8), Engine::Fill), None).unwrap();
        job.complete_region(0, &a_patch(&plain, Rect::new(40, 0, 12, 8), Engine::Fill), None).unwrap();
        let detection = a_stored_detection(&mut job, &page, Rect::new(20, 10, 8, 6));
        invalidate_manifest_cache(&path);
        let project = Job::open(&path).unwrap().project;
        let flags = |ids: &[&str]| -> Vec<(String, Option<&'static str>)> {
            let (pages, _) = pages_and_review(&chapter.id, &project, &[0]);
            ids.iter().map(|id| {
                let region = pages[0].regions.iter().find(|region| region.id == *id).unwrap();
                (region.outcome.to_owned(), review_reason(region))
            }).collect()
        };
        let (first, retry, layer_attempt) = (format!("{detection}-a1"), format!("{detection}-a2"), format!("{layer}-a1"));

        replace_cloud_attention(CloudAttentionScan {
            flags: vec![
                (detection.clone(), first.clone(), "review.reason.cloudResultNotApplied"),
                (detection.clone(), retry.clone(), "review.reason.repairNeeded"),
                (layer.clone(), layer_attempt.clone(), "review.reason.repairNeeded"),
            ],
            unjudged: vec![],
        });
        assert_eq!(flags(&[&detection, &layer, &plain]), [
            ("detected".to_owned(), Some("review.reason.repairNeeded")),
            ("cleaned".to_owned(), Some("review.reason.repairNeeded")),
            ("cleaned".to_owned(), None),
        ], "the most urgent reason, on a detection and on a layer; an unflagged layer stays clean");
        let (pages, index) = pages_and_review(&chapter.id, &project, &[0]);
        assert_eq!((pages[0].done_count, pages[0].review_count), (1, 2));
        let mut indexed: Vec<_> = index.iter().map(|entry| entry.id.clone()).collect();
        indexed.sort();
        let mut expected = vec![detection.clone(), layer.clone()];
        expected.sort();
        assert_eq!(indexed, expected);

        forget_repair(&retry);
        assert_eq!(flags(&[&detection])[0].1, Some("review.reason.cloudResultNotApplied"),
            "settling one attempt leaves the other's flag");

        // A read that could not judge the layer's attempt keeps its flag, and
        // drops the detection's, which it judged and found nothing on.
        replace_cloud_attention(CloudAttentionScan { flags: vec![], unjudged: vec![layer_attempt.clone()] });
        assert_eq!(flags(&[&detection, &layer]).into_iter().map(|(_, reason)| reason).collect::<Vec<_>>(),
            [None, Some("review.reason.repairNeeded")]);

        forget_repair(&layer_attempt);
        replace_cloud_attention(CloudAttentionScan::default());
        let (pages, index) = pages_and_review(&chapter.id, &project, &[0]);
        assert_eq!(pages[0].review_count, 0);
        assert!(index.is_empty());
    }

    /// The Layers row's Delete on a detection goes through the command's own
    /// path - observe, then delete what was observed - and takes it off the page.
    #[test]
    fn deleting_a_detection_through_the_command_path_removes_it() {
        let scratch = Scratch::new("delete-detection-command");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let page = page_id(&chapter.id, 0);
        let mut job = Job::open(&path).unwrap();
        let id = a_stored_detection(&mut job, &page, Rect::new(20, 10, 8, 6));
        invalidate_manifest_cache(&path);
        let mask_id = library.load_pages(&chapter.id, &[0]).unwrap()[0].regions.iter()
            .find(|region| region.id == id).and_then(|region| region.mask.as_ref())
            .map(|mask| mask.id.clone()).unwrap();
        let region_id = region_of_mask(&mask_id);
        let observed = library.observe_mask(region_id).unwrap().expect("the detection is observed");
        let (_, status) = library.delete_observed_mask(region_id, observed).unwrap().expect("deleted");
        assert_eq!(status, "unclean");
        assert!(library.load_pages(&chapter.id, &[0]).unwrap()[0].regions.iter().all(|region| region.id != id));
    }

    #[test]
    fn queued_detection_delete_does_not_hide_a_replacement_patch() {
        let scratch = Scratch::new("delete-detection-race");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let page = page_id(&chapter.id, 0);
        let mut job = Job::open(&path).unwrap();
        let id = a_stored_detection(&mut job, &page, Rect::new(20, 10, 8, 6));
        let observed = library.observe_mask(&id).unwrap().unwrap();
        let patch = a_patch(&id, Rect::new(20, 10, 8, 6), Engine::Fill);
        assert!(job.complete_detection(&id, &patch, None).unwrap());
        assert_eq!(library.delete_observed_mask(&id, observed).unwrap_err(), "mask_changed");
        let reopened = Job::open(&path).unwrap();
        assert!(reopened.project.patches.iter().any(|row| row.id == id && row.visible));
    }

    #[test]
    fn deleting_a_mask_takes_it_off_the_page_and_undoing_puts_it_back() {
        let scratch = Scratch::new("delete-mask");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 2);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let path = library.resolve_chapter(&chapter.id).unwrap();
        let region = format!("{}-r0", page_id(&chapter.id, 0));
        let mut job = Job::open(&path).unwrap();
        job.complete_region(
            0,
            &a_patch(&region, Rect::new(0, 0, 12, 8), Engine::Fill),
            None,
        )
        .unwrap();

        // The seam addresses the mask, never the region: `<region id>-m1`.
        let (deleted, status) = library
            .set_mask_visible(region_of_mask(&format!("{region}-m1")), false)
            .unwrap()
            .unwrap();
        assert_eq!(deleted.id, region);
        assert!(
            deleted.mask.is_none(),
            "a deleted mask is still on the region"
        );
        assert_eq!(
            deleted.outcome, "pending",
            "a region with no mask is back in the queue"
        );
        assert_eq!(
            status, "unclean",
            "the page kept a cleaned mark with nothing applied"
        );

        // Persisted, and the composite can no longer see it - which is what
        // takes the fill off the page the user is looking at.
        let reopened = Job::open(&path).unwrap();
        let record = reopened
            .project
            .patches
            .iter()
            .find(|r| r.id == region)
            .unwrap();
        assert!(!record.visible);
        assert!(
            reopened.sidecar().join(&record.buffer_ref).exists(),
            "the buffers went with the delete, so the undo cannot be served"
        );
        assert_eq!(
            library.list_projects().unwrap()[0].chapters[0].pages[0].status,
            "unclean"
        );

        // And undo is the same call the other way.
        let (restored, status) = library.set_mask_visible(&region, true).unwrap().unwrap();
        assert!(restored.mask.is_some());
        assert_eq!(restored.outcome, "cleaned");
        assert_eq!(status, "cleaned");
    }

    /// Undoing the creation of a hand region removes its patch record and buffer
    /// files completely, leaving no row when reopened.
    #[test]
    fn undoing_a_created_region_removes_the_record_and_its_buffers() {
        let scratch = Scratch::new("create-then-undo");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let path = library.resolve_chapter(&chapter.id).unwrap();
        let region = format!("{}-h1", page_id(&chapter.id, 0));
        let mut job = Job::open(&path).unwrap();
        job.complete_region(
            0,
            &a_patch(&region, Rect::new(0, 0, 12, 8), Engine::Fill),
            None,
        )
        .unwrap();

        let reopened = Job::open(&path).unwrap();
        let record = reopened
            .project
            .patches
            .iter()
            .find(|r| r.id == region)
            .unwrap();
        let buf_path = reopened.sidecar().join(&record.buffer_ref);
        assert!(buf_path.exists());

        // Undo creation removes the region entirely
        library.remove_region(&region).unwrap();

        let after_undo = Job::open(&path).unwrap();
        assert!(after_undo.project.patches.iter().all(|r| r.id != region));
        assert!(!buf_path.exists());

        let page = page_of(&chapter.id, &after_undo.project, 0).unwrap();
        assert!(page.regions.iter().all(|r| r.id != region));
        assert_eq!(page.region_count, 0);
    }

    /// Chapter ids are minted from one counter and are not prefix-free, so the
    /// lookup a region-level edit depends on has to prefer the longest match.
    #[test]
    fn a_region_id_is_matched_to_the_longest_chapter_prefix_and_not_the_first() {
        let mut index = Index::default();
        index.projects.push(IndexProject {
            id: "p1".into(),
            name: "P".into(),
            mode: StripMode::Single,
            source_path: None,
            reading_direction: ReadingDirection::default(),
            created: 0,
            last_opened: 0,
            starred: false,
            cloud_consent: None,
            chapters: vec![
                IndexChapter {
                    id: "ch1".into(),
                    name: "One".into(),
                    number: 1,
                    source_path: None,
                    created: 0,
                    last_opened: 0,
                },
                IndexChapter {
                    id: "ch12".into(),
                    name: "Twelve".into(),
                    number: 12,
                    source_path: None,
                    created: 0,
                    last_opened: 0,
                },
            ],
        });
        assert_eq!(
            chapter_holding(&index, "ch12-p001-r0").as_deref(),
            Some("ch12")
        );
        assert_eq!(
            chapter_holding(&index, "ch1-p001-r0").as_deref(),
            Some("ch1")
        );
        assert_eq!(chapter_holding(&index, "ch3-p001-r0"), None);
    }

    /* ---------- the resident window ---------- */

    /// Opening a chapter does not bring its regions with it, and the window is
    /// asked for separately. The point of the split is exactly this: a 200-page
    /// chapter costs a 200-entry header list to open, whatever is on the pages.
    #[test]
    fn opening_a_chapter_brings_headers_and_a_window_brings_regions() {
        let scratch = Scratch::new("window");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 4);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        for page in 0..4 {
            job.complete_region(
                page,
                &a_patch(&format!("r{page}"), Rect::new(0, 0, 8, 8), Engine::Fill),
                None,
            )
            .unwrap();
        }

        let opened = library
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        assert_eq!(opened.chapter.pages.len(), 4);
        for page in &opened.chapter.pages {
            assert!(!page.resident, "page {} arrived resident", page.index);
            assert!(page.regions.is_empty());
            assert_eq!(page.region_count, 1, "a header still counts");
        }

        // Three pages, which is the whole of the window: previous, current,
        // next.
        let window = library.load_pages(&chapter.id, &[0, 1, 2]).unwrap();
        assert_eq!(window.len(), 3);
        assert!(window
            .iter()
            .all(|page| page.resident && page.regions.len() == 1));
        assert_eq!(window[2].index, 2);
    }

    /// A page index that is not there is not an error - the window slides past
    /// both ends of the chapter and clamping it in two places is how the two
    /// come to disagree.
    #[test]
    fn a_window_past_the_end_answers_with_what_is_there() {
        let scratch = Scratch::new("window-edge");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 2);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let window = library.load_pages(&chapter.id, &[1, 2, 9]).unwrap();
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].index, 1);
    }

    /// The review set is chapter-wide and the regions are not, so the index has
    /// to answer for pages nobody has paged in - and it has to answer exactly
    /// what `src/lib/model/review.js` would have said about the same regions.
    #[test]
    fn the_review_index_is_chapter_wide_without_the_chapter_being_resident() {
        let scratch = Scratch::new("review-index");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 3);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        job.complete_region(
            0,
            &a_patch("r1", Rect::new(0, 0, 8, 8), Engine::Fill),
            Some("review.reason.fittingReconstructed".into()),
        )
        .unwrap();
        // Cleaned and unremarkable: not an issue, and it must not be listed.
        job.complete_region(1, &a_patch("r2", Rect::new(0, 0, 8, 8), Engine::Fill), None)
            .unwrap();
        job.leave_untouched(
            2,
            Rect::new(4, 4, 6, 6),
            "review.reason.gateSkippedOutsideBubble",
        )
        .unwrap();

        let opened = library
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        let review = &opened.chapter.review;
        assert!(opened.chapter.pages.iter().all(|page| !page.resident));
        assert_eq!(review.len(), 2);
        assert_eq!(review[0].reason_key, "review.reason.fittingReconstructed");
        assert_eq!(review[0].page_index, 0);
        assert_eq!(
            review[1].reason_key,
            "review.reason.gateSkippedOutsideBubble"
        );
        assert_eq!(review[1].page_index, 2);
        assert_eq!(review[1].page_id, page_id(&chapter.id, 2));
    }

    /// The index and `review.js` are two implementations of one rule, so the
    /// index is checked against the regions themselves rather than against a
    /// list written out by hand.
    #[test]
    fn the_review_index_is_review_reason_over_the_same_regions() {
        let scratch = Scratch::new("review-parity");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 2);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        job.complete_region(
            0,
            &a_patch("r1", Rect::new(0, 0, 8, 8), Engine::Cloud),
            Some("review.reason.cloudRejectedSafetyFilter".into()),
        )
        .unwrap();
        job.complete_region(
            0,
            &a_patch("r2", Rect::new(0, 0, 8, 8), Engine::Cloud),
            Some("review.reason.cloudAccepted".into()),
        )
        .unwrap();
        job.complete_region(
            0,
            &a_patch("r3", Rect::new(0, 0, 8, 8), Engine::Flux),
            Some("review.reason.checkGeneratedTexture".into()),
        )
        .unwrap();
        job.leave_untouched(1, Rect::new(2, 2, 4, 4), "review.reason.declined")
            .unwrap();

        let opened = library
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        let pages = library.load_pages(&chapter.id, &[0, 1]).unwrap();
        let expected: Vec<_> = pages
            .iter()
            .flat_map(|page| page.regions.iter())
            .filter_map(|region| review_reason(region).map(|key| (region.id.clone(), key)))
            .collect();
        let indexed: Vec<_> = opened
            .chapter
            .review
            .iter()
            .map(|r| (r.id.clone(), r.reason_key))
            .collect();
        assert_eq!(indexed, expected);
        // The legacy accepted flag is migrated away; the texture check remains.
        assert_eq!(indexed.len(), 3);
    }

    /// A deleted mask leaves **everything**: the review index, the page's
    /// regions, its counts and its status. Nothing lists an invisible record,
    /// so the row the user deleted does not come back as an empty one - which
    /// is what it did while `region_of_deleted_patch` was a listing shape.
    #[test]
    fn a_deleted_mask_leaves_the_page_and_the_review_index() {
        let scratch = Scratch::new("review-deleted");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        // A region id the library can route: `chapter_holding` recovers the
        // chapter from the id alone, and it does that by prefix.
        let region_id = format!("{}-p001-r0", chapter.id);
        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        job.complete_region(
            0,
            &a_patch(&region_id, Rect::new(0, 0, 8, 8), Engine::Fill),
            Some("review.reason.fittingReconstructed".into()),
        )
        .unwrap();
        drop(job);

        let before = library
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        assert_eq!(before.chapter.review.len(), 1);

        library.set_mask_visible(&region_id, false).unwrap();
        let after = library
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        assert!(after.chapter.review.is_empty());
        assert_eq!(
            after.chapter.pages[0].region_count, 0,
            "an empty row was left behind"
        );
        assert_eq!(after.chapter.pages[0].status, "unclean");

        let loaded = library.load_pages(&chapter.id, &[0]).unwrap();
        assert!(
            loaded[0].regions.is_empty(),
            "the deleted region is still listed"
        );

        // And undo puts the row back where it was, review flag and all.
        library.set_mask_visible(&region_id, true).unwrap();
        let undone = library.load_pages(&chapter.id, &[0]).unwrap();
        assert_eq!(undone[0].regions.len(), 1);
        assert!(undone[0].regions[0].mask.is_some());
        assert_eq!(undone[0].status, "cleaned");
    }

    #[test]
    fn layer_style_survives_reopen_and_restores_previous_snapshot() {
        let scratch = Scratch::new("layer-style-reopen");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let region_id = format!("{}-r0", page_id(&chapter.id, 0));
        let original_bounds = Rect::new(8, 8, 8, 6);
        let mut job = Job::open(&path).unwrap();
        job.complete_region(0, &a_patch(&region_id, original_bounds, Engine::Paint), None).unwrap();
        drop(job);

        let moved = cleaner_core::patch::LayerStyle {
            opacity: 42, offset_x: 20, offset_y: 12,
            rotation: 90.0, locked: true,
        };
        let edited = library.set_layer_style(&region_id, moved, LayerEdit::Edit).unwrap().unwrap().0;
        assert_eq!(edited.mask.as_ref().unwrap().layer, moved);
        let reopened = Job::open(&path).unwrap();
        let patch = reopened.load_patch(&reopened.project.patches[0]).unwrap();
        assert_eq!(patch.layer_style(), moved);
        assert_ne!(patch.mask.bounds, original_bounds);
        drop(reopened);

        let restored = library.set_layer_style(&region_id, cleaner_core::patch::LayerStyle::default(), LayerEdit::Restore)
            .unwrap().unwrap().0;
        assert_eq!(restored.mask.as_ref().unwrap().layer.opacity, 100);
        let reopened = Job::open(&path).unwrap();
        assert_eq!(reopened.load_patch(&reopened.project.patches[0]).unwrap().mask.bounds, original_bounds);
    }

    /// One chapter with one patch made by `engine`, and its region id.
    fn a_layer(scratch: &Scratch, engine: Engine) -> (Library, String, PathBuf, String) {
        let library = library(scratch);
        let project = a_project(&library, scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let region_id = format!("{}-r0", page_id(&chapter.id, 0));
        let mut job = Job::open(&path).unwrap();
        job.complete_region(0, &a_patch(&region_id, Rect::new(8, 8, 8, 6), engine), None).unwrap();
        drop(job);
        invalidate_manifest_cache(&path);
        (library, chapter.id, path, region_id)
    }

    #[test]
    fn orientation_layer_style_moves_in_visible_axes_and_restores_native_history() {
        for value in 1..=8 {
            let scratch=Scratch::new(&format!("orientation-layer-style-{value}"));
            let (library,chapter,path,id)=a_layer(&scratch,Engine::Paint);
            let mut job=Job::open(&path).unwrap();job.project.sources[0].orientation=cleaner_core::image::orientation::Orientation(value);job.flush().unwrap();
            let source=job.project.sources[0].clone();let original=job.project.patches[0].bbox;
            let desired=cleaner_core::patch::LayerStyle { offset_x:3,offset_y:4,rotation:90.0,opacity:60,locked:false };
            let (region,_)=library.set_layer_style(&id,desired,LayerEdit::Edit).unwrap().unwrap();
            assert_eq!(region.mask.unwrap().layer,desired,"orientation {value}");
            let opened=Job::open(&path).unwrap();let record=&opened.project.patches[0];
            let expected=desired.display_bounds(source.orientation.rect(original,source.w,source.h));
            assert_eq!(opened.load_display_patch(record).unwrap().mask.bounds,expected,"orientation {value}");
            assert_eq!(cleaner_core::project::orientation::display_bbox(&opened.project,record),expected);
            library.set_layer_style(&id,Default::default(),LayerEdit::Restore).unwrap();
            let restored=library.load_pages(&chapter,&[0]).unwrap().remove(0);
            assert_eq!(restored.regions[0].mask.as_ref().unwrap().layer,Default::default());
        }
    }

    fn styled(opacity: u8, offset_x: i32, rotation: f64, locked: bool) -> cleaner_core::patch::LayerStyle {
        cleaner_core::patch::LayerStyle { opacity, offset_x, offset_y: 0, rotation, locked }
    }

    /// The capabilities cross the seam with the layer, and the command refuses
    /// what they do not allow - by catalogue key, with the manifest untouched.
    #[test]
    fn a_redraw_is_fixed_natively_and_a_fill_moves() {
        use cleaner_core::patch::{LayerPlacement, LayerRefusal};
        let scratch = Scratch::new("layer-capabilities");
        let (library, chapter_id, path, region_id) = a_layer(&scratch, Engine::Lama);
        let page = library.load_pages(&chapter_id, &[0]).unwrap().remove(0);
        let mask = page.regions[0].mask.clone().unwrap();
        assert_eq!(mask.capabilities.transform, LayerPlacement::Fixed);
        assert!(!mask.capabilities.lock && mask.capabilities.opacity);
        let json = serde_json::to_value(&mask).unwrap();
        assert_eq!(json["capabilities"], serde_json::json!({"transform": "fixed", "lock": false, "opacity": true}));
        assert!(json["appearance"].as_str().is_some_and(|digest| digest.len() == 16));
        assert_eq!(json["sourceBbox"], serde_json::to_value(page.regions[0].bbox).unwrap());

        let moved = library.set_layer_style(&region_id, styled(100, 12, 0.0, false), LayerEdit::Edit).unwrap_err();
        assert!(matches!(moved, LibraryError::LayerRefused(LayerRefusal::Fixed)));
        assert_eq!(String::from(moved), "masks.refused.fixed");
        let turned = library.set_layer_style(&region_id, styled(100, 0, 30.0, false), LayerEdit::Edit).unwrap_err();
        assert!(matches!(turned, LibraryError::LayerRefused(LayerRefusal::Fixed)));
        let locked = library.set_layer_style(&region_id, styled(100, 0, 0.0, true), LayerEdit::Edit).unwrap_err();
        assert!(matches!(locked, LibraryError::LayerRefused(LayerRefusal::NoLock)));
        let job = Job::open(&path).unwrap();
        assert!(job.project.patches[0].provenance.params_snapshot.get("layer").is_none(), "a refusal wrote");
        drop(job);

        let faded = library.set_layer_style(&region_id, styled(50, 0, 0.0, false), LayerEdit::Edit).unwrap().unwrap().0;
        assert_eq!(faded.mask.unwrap().layer.opacity, 50);

        let scratch = Scratch::new("layer-capabilities-fill");
        let (library, _, _, region_id) = a_layer(&scratch, Engine::Fill);
        let moved = library.set_layer_style(&region_id, styled(100, 12, 30.0, false), LayerEdit::Edit).unwrap().unwrap().0;
        let mask = moved.mask.unwrap();
        assert_eq!(mask.capabilities.transform, LayerPlacement::Movable);
        assert_eq!((mask.layer.offset_x, mask.layer.rotation), (12, 30.0));
        assert_ne!(moved.bbox, mask.source_bbox, "the drawn box did not follow the move");
        library.set_layer_style(&region_id, styled(100, 12, 30.0, true), LayerEdit::Edit).unwrap();
        let refused = library.set_layer_style(&region_id, styled(100, 40, 30.0, true), LayerEdit::Edit).unwrap_err();
        assert!(matches!(refused, LibraryError::LayerRefused(LayerRefusal::Locked)));
    }

    /// A redraw moved before the rule existed keeps that geometry: opacity
    /// still edits it, a move does not, and history replay neither resets nor
    /// fails.
    #[test]
    fn a_legacy_transformed_redraw_keeps_its_frozen_geometry() {
        let scratch = Scratch::new("layer-legacy-frozen");
        let (library, chapter_id, path, region_id) = a_layer(&scratch, Engine::Flux);
        let mut job = Job::open(&path).unwrap();
        job.project.patches[0].provenance.params_snapshot["layer"] =
            serde_json::json!({"offsetX": 20, "offsetY": 12, "rotation": 90.0, "locked": true});
        job.flush().unwrap();
        drop(job);
        invalidate_manifest_cache(&path);
        let before = library.load_pages(&chapter_id, &[0]).unwrap().remove(0);

        let faded = library.set_layer_style(
            &region_id,
            cleaner_core::patch::LayerStyle { opacity: 40, offset_x: 20, offset_y: 12, rotation: 90.0, locked: true },
            LayerEdit::Edit,
        ).unwrap().unwrap().0;
        assert_eq!(faded.bbox, before.regions[0].bbox, "the frozen geometry moved");
        assert!(library.set_layer_style(&region_id, cleaner_core::patch::LayerStyle::default(), LayerEdit::Edit).is_err());

        let replayed = library.set_layer_style(&region_id, cleaner_core::patch::LayerStyle::default(), LayerEdit::Restore)
            .unwrap().unwrap().0;
        let layer = replayed.mask.unwrap().layer;
        assert_eq!((layer.opacity, layer.offset_x, layer.offset_y, layer.rotation), (100, 20, 12, 90.0));
        assert_eq!(replayed.bbox, before.regions[0].bbox);
    }

    #[test]
    fn a_detection_refuses_output_opacity_and_placement() {
        use cleaner_core::patch::{LayerPlacement, LayerRefusal};
        let scratch = Scratch::new("layer-detection");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None)
            .unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let mut job = Job::open(&path).unwrap();
        let id = a_stored_detection(&mut job, &page_id(&chapter.id, 0), Rect::new(20, 10, 8, 6));
        drop(job);
        invalidate_manifest_cache(&path);

        let page = library.load_pages(&chapter.id, &[0]).unwrap().remove(0);
        let mask = page.regions[0].mask.clone().unwrap();
        assert_eq!(mask.capabilities.transform, LayerPlacement::None);
        assert!(!mask.capabilities.opacity && !mask.capabilities.lock);
        assert!(mask.appearance.is_none());
        let refused = library.set_layer_style(&id, styled(50, 0, 0.0, false), LayerEdit::Edit).unwrap_err();
        assert!(matches!(refused, LibraryError::LayerRefused(LayerRefusal::NoOutput)));
        let refused = library.set_layer_style(&id, styled(100, 5, 0.0, false), LayerEdit::Edit).unwrap_err();
        assert_eq!(refused.to_string(), "masks.refused.noOutput");
        assert!(library.set_layer_style(&id, styled(50, 0, 0.0, false), LayerEdit::Restore).unwrap().is_none());
    }

    /// The cache identity the tile URL carries: it moves with opacity,
    /// geometry and visibility, stays put for a lock and for a write that
    /// changes nothing, comes back when an undo does, and survives a reopen.
    #[test]
    fn page_appearance_follows_what_the_page_shows_and_survives_reopen() {
        let scratch = Scratch::new("layer-appearance");
        let (library, chapter_id, path, region_id) = a_layer(&scratch, Engine::Paint);
        let read = || {
            invalidate_manifest_cache(&path);
            let page = library.load_pages(&chapter_id, &[0]).unwrap().remove(0);
            let mask = page.regions.first().and_then(|region| region.mask.clone());
            (page.appearance.clone(), mask.and_then(|mask| mask.appearance))
        };
        let original = read();
        assert_eq!(original.0.len(), 16);

        library.set_layer_style(&region_id, styled(50, 0, 0.0, false), LayerEdit::Edit).unwrap();
        let half = read();
        assert_ne!(half.0, original.0, "opacity did not move the page identity");
        assert_ne!(half.1, original.1, "opacity did not move the layer identity");

        library.set_layer_style(&region_id, styled(50, 0, 0.0, true), LayerEdit::Edit).unwrap();
        assert_eq!(read(), half, "a lock changed the identity of unchanged pixels");
        library.set_layer_style(&region_id, styled(50, 0, 0.0, false), LayerEdit::Edit).unwrap();

        library.set_layer_style(&region_id, styled(50, 6, 0.0, false), LayerEdit::Edit).unwrap();
        let moved = read();
        assert_ne!(moved.0, half.0);
        library.set_layer_style(&region_id, styled(50, 6, 15.0, false), LayerEdit::Edit).unwrap();
        assert_ne!(read().0, moved.0, "a rotation did not move the identity");

        // Undo all the way back: the same saved state answers the same URL.
        library.set_layer_style(&region_id, styled(100, 0, 0.0, false), LayerEdit::Restore).unwrap();
        assert_eq!(read().0, original.0);

        library.set_mask_visible(&region_id, false).unwrap();
        let hidden = read();
        assert_ne!(hidden.0, original.0, "a hidden layer kept the shown identity");
        library.set_mask_visible(&region_id, true).unwrap();
        assert_eq!(read().0, original.0);

        // Reopened by a new library over the same files: the same identity.
        let again = Library::at(scratch.join("library"));
        let page = again.load_pages(&chapter_id, &[0]).unwrap().remove(0);
        assert_eq!(page.appearance, original.0);
        let reopened = Job::open(&path).unwrap();
        let header = page_of(&chapter_id, &reopened.project, 0).unwrap();
        assert_eq!(header.appearance, original.0, "a page header disagreed with the resident page");
    }

    /// Home and a chapter opening list every page of a chapter, and each
    /// header carries its appearance. A long strip pays for its records once,
    /// not once per page: a hundred pages under a thousand layers, a tenth of
    /// them hanging across a join, list in under half a second in a debug
    /// build (about 70 ms on the machine this was written on).
    #[test]
    fn a_hundred_page_strip_with_a_thousand_layers_lists_quickly() {
        let scratch = Scratch::new("appearance-scale");
        let library = library(&scratch);
        let source = scans(&scratch.join("raws"), 100);
        let project = library.create_project("Long Road", StripMode::Longstrip, Some(source), None).unwrap();
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None).unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let mut job = Job::open(&path).unwrap();
        assert_eq!(job.project.strip.mode, StripMode::Longstrip);
        assert_eq!(job.project.strip.order.len(), 100);
        let template = PatchRecord::of(0, &a_patch("template", Rect::new(4, 4, 8, 8), Engine::Fill), None);
        for n in 0..1000usize {
            let page = n % 100;
            let mut record = template.clone();
            record.id = format!("{}-r{n}", page_id(&chapter.id, page));
            record.source_idx = job.project.strip.order[page];
            record.order = n as u32;
            record.bbox = if n % 10 == 0 { Rect::new(4, 40, 8, 20) } else { Rect::new(4, (n % 30) as i64, 8, 8) };
            job.project.patches.push(record);
        }

        let started = std::time::Instant::now();
        let (pages, _) = pages_and_review(&chapter.id, &job.project, &[]);
        let listed = started.elapsed();
        assert_eq!(pages.len(), 100);
        for index in [0, 1, 50, 98, 99] {
            assert_eq!(pages[index].appearance, crate::tile::page_appearance(&job.project, index), "page {index}");
        }
        assert!(listed < std::time::Duration::from_millis(500), "100 pages x 1000 layers listed in {listed:?}");
    }

    /// The redo of a delete and the undo of a hand-drawn creation arrive as the
    /// same call - a snapshot with no region - and a patch record answers it by
    /// going invisible rather than by being destroyed. A hard removal would
    /// serve the creation and break the delete: the undo after a redo would
    /// have nothing left to turn back on.
    #[test]
    fn a_null_snapshot_softens_a_patch_rather_than_destroying_it() {
        let scratch = Scratch::new("restore-null-patch");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let path = library.resolve_chapter(&chapter.id).unwrap();
        let region = format!("{}-r0", page_id(&chapter.id, 0));
        let mut job = Job::open(&path).unwrap();
        job.complete_region(
            0,
            &a_patch(&region, Rect::new(0, 0, 12, 8), Engine::Fill),
            None,
        )
        .unwrap();
        drop(job);

        // Delete, undo, redo - the redo is `restoreRegion` with no region, and
        // the command's null branch is this pair of calls.
        library.set_mask_visible(&region, false).unwrap().unwrap();
        library.set_mask_visible(&region, true).unwrap().unwrap();
        assert!(
            library.set_mask_visible(&region, false).unwrap().is_some(),
            "the redo found nothing"
        );

        let reopened = Job::open(&path).unwrap();
        let record = reopened
            .project
            .patches
            .iter()
            .find(|r| r.id == region)
            .unwrap();
        assert!(!record.visible);
        assert!(
            reopened.sidecar().join(&record.buffer_ref).exists(),
            "a redone delete destroyed the buffers the next undo needs"
        );

        // Which is why the undo after the redo still works.
        let (restored, status) = library.set_mask_visible(&region, true).unwrap().unwrap();
        assert!(restored.mask.is_some());
        assert_eq!(status, "cleaned");
    }

    /// **Auto clean over a page whose masks were deleted.** The page was
    /// examined by the run that made them, and `runnable` reads exactly that
    /// field - so a second Auto clean does not queue the page and the deletions
    /// stand. Nothing revives, nothing crashes, and the user's delete is not
    /// undone by a button that says "clean". A page the run never finished is
    /// queued as before, and `reset_page` drops its `-r` rows - invisible ones
    /// included - so the re-detection starts from a clean slate.
    #[test]
    fn a_second_auto_clean_does_not_revive_a_deleted_mask() {
        let scratch = Scratch::new("rerun-deleted");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 2);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let path = library.resolve_chapter(&chapter.id).unwrap();
        let region = format!("{}-r0", page_id(&chapter.id, 0));
        let mut job = Job::open(&path).unwrap();
        job.complete_region(
            0,
            &a_patch(&region, Rect::new(0, 0, 12, 8), Engine::Fill),
            None,
        )
        .unwrap();
        job.mark_examined(0).unwrap();
        drop(job);
        invalidate_manifest_cache(&path);

        library.set_mask_visible(&region, false).unwrap().unwrap();

        let queued: Vec<usize> = crate::run::plan(&library, "chapter", &chapter.id, None)
            .unwrap()
            .iter()
            .map(|entry| entry.page_index)
            .collect();
        assert_eq!(
            queued,
            vec![1],
            "the examined page was queued again by the deletion"
        );
        assert!(
            crate::run::plan(&library, "page", &chapter.id, Some(0))
                .unwrap()
                .is_empty(),
            "a page-scoped rerun revived the page the user deleted from"
        );

        let after = library.load_pages(&chapter.id, &[0]).unwrap();
        assert!(after[0].regions.is_empty());
    }

    /// The Pages list draws `done / total` and a `✓ n` mark for **every** page
    /// of the chapter, and only three of them are ever resident. So the counts
    /// ride on the header, they are read out of the manifest, and a second
    /// `Library` over the same folder - the app restarted, the project reopened
    /// - reports the same three numbers without a page having been loaded.
    #[test]
    fn every_page_header_counts_done_and_flagged_and_survives_a_reopen() {
        let scratch = Scratch::new("header-counts");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 3);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        // Page 0: one finished region, one flagged one.
        job.complete_region(0, &a_patch("r0", Rect::new(0, 0, 8, 8), Engine::Fill), None)
            .unwrap();
        job.complete_region(
            0,
            &a_patch("r1", Rect::new(9, 9, 8, 8), Engine::Fill),
            Some("review.reason.fittingReconstructed".into()),
        )
        .unwrap();
        // Page 1: one finished region and nothing else.
        job.complete_region(1, &a_patch("r2", Rect::new(0, 0, 8, 8), Engine::Fill), None)
            .unwrap();
        // Page 2: the gate held one back. Flagged, and finished with nothing.
        job.leave_untouched(
            2,
            Rect::new(4, 4, 6, 6),
            "review.reason.gateSkippedOutsideBubble",
        )
        .unwrap();
        drop(job);

        let expected = [(2u32, 1u32, 1u32), (1, 1, 0), (1, 0, 1)];
        let counted = |pages: &[ApiPage]| -> Vec<(u32, u32, u32)> {
            pages
                .iter()
                .map(|p| (p.region_count, p.done_count, p.review_count))
                .collect()
        };

        let opened = library
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        assert!(opened
            .chapter
            .pages
            .iter()
            .all(|page| !page.resident && page.regions.is_empty()));
        assert_eq!(
            counted(&opened.chapter.pages),
            expected,
            "headers count nothing"
        );

        // The same numbers on the page that *is* loaded: the header is the
        // resident page with its regions shed, not a second opinion.
        let window = library.load_pages(&chapter.id, &[0]).unwrap();
        assert_eq!(counted(&window), expected[..1]);

        // Reopened cold: a fresh `Library` over the same folder, with nothing
        // cached in front of the manifest.
        invalidate_manifest_cache(&library.resolve_chapter(&chapter.id).unwrap());
        let reopened = Library::at(scratch.join("library"))
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            counted(&reopened.chapter.pages),
            expected,
            "counts did not survive a reopen"
        );
    }

    /// An edit lands on a page the window is not holding - the reader is
    /// eleven pages away, or the app has just reopened - and the row for that
    /// page has to move anyway. It does because the counts are the manifest's,
    /// so writing the manifest is what changes them.
    #[test]
    fn an_edit_moves_the_counts_of_a_page_no_window_is_holding() {
        let scratch = Scratch::new("header-counts-edit");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 4);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let region_id = format!("{}-p004-r0", chapter.id);
        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        job.complete_region(
            3,
            &a_patch(&region_id, Rect::new(0, 0, 8, 8), Engine::Fill),
            None,
        )
        .unwrap();
        drop(job);

        let before = library
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                before.chapter.pages[3].region_count,
                before.chapter.pages[3].done_count
            ),
            (1, 1)
        );

        // Only pages 0..=2 are ever loaded; page 3 is edited while nobody holds it.
        library.load_pages(&chapter.id, &[0, 1, 2]).unwrap();
        library.set_mask_visible(&region_id, false).unwrap();

        let after = library
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        let page = &after.chapter.pages[3];
        assert!(!page.resident);
        assert_eq!(
            page.region_count, 0,
            "the deleted region is still counted as a row"
        );
        assert_eq!(
            page.done_count, 0,
            "a deleted mask is not a finished region"
        );
        assert_eq!(page.review_count, 0, "and it is not an issue either");
        assert_eq!(page.status, "unclean");
    }

    /// `regions_untouched` rows have no id of their own, so one is derived from
    /// the bbox - and it has to be derived from the *whole* bbox. Two
    /// detections that start at the same pixel and run to different sizes are
    /// two regions; a corner-only id merged them, and the Layers panel would
    /// show one where the manifest holds two.
    #[test]
    fn two_untouched_regions_sharing_a_corner_are_two_regions() {
        let scratch = Scratch::new("untouched-ids");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        job.leave_untouched(0, Rect::new(6, 6, 10, 10), "review.reason.declined")
            .unwrap();
        job.leave_untouched(
            0,
            Rect::new(6, 6, 24, 4),
            "review.reason.gateSkippedNotJapanese",
        )
        .unwrap();

        let pages = library.load_pages(&chapter.id, &[0]).unwrap();
        let regions = &pages[0].regions;
        assert_eq!(regions.len(), 2);
        assert_ne!(
            regions[0].id, regions[1].id,
            "two regions collapsed onto one id"
        );
        // The whole rectangle is what separates them, and it is what an id
        // derived from geometry has to carry.
        assert!(regions[0].id.ends_with("-u6-6-10-10"), "{}", regions[0].id);
        assert!(regions[1].id.ends_with("-u6-6-24-4"), "{}", regions[1].id);
    }

    #[test]
    fn duplicate_legacy_untouched_rows_expose_one_latest_region_without_rewriting_job() {
        let scratch=Scratch::new("duplicate-untouched");
        let library=library(&scratch);
        let project=a_project(&library,&scratch,1);
        let chapter=library.create_chapter(&project.id,"Ch 1",None,None).unwrap().created().unwrap();
        let path=library.resolve_chapter(&chapter.id).unwrap();
        let mut job=Job::open(&path).unwrap();
        let bbox=Rect::new(6,6,10,10);
        job.leave_untouched(0,bbox,"review.reason.declined").unwrap();
        // Reproduce legacy append-only records without changing their provenance.
        let mut duplicate=job.project.regions_untouched[0].clone();
        duplicate.reason="review.reason.gateSkippedNotJapanese".into();
        job.project.regions_untouched.push(duplicate);
        job.flush().unwrap();
        let before=std::fs::read(&path).unwrap();
        let loaded=library.load_pages(&chapter.id,&[0]).unwrap();
        assert_eq!(loaded[0].regions.len(),1);
        assert_eq!(loaded[0].regions[0].gate_skip_cause,Some("not-japanese"));
        assert_eq!((loaded[0].region_count,loaded[0].review_count),(1,1));
        let opened=library.open_chapter(&project.id,&chapter.id,false).unwrap().unwrap();
        assert_eq!((opened.chapter.pages[0].region_count,opened.chapter.pages[0].review_count),(1,1));
        assert_eq!(opened.chapter.review.len(),1);
        assert_eq!(std::fs::read(&path).unwrap(),before,"reading must preserve duplicate legacy records");
        assert_eq!(Job::open(&path).unwrap().project.regions_untouched.len(),2);
    }

    /* ---------- the thread a command runs on ---------- */

    /// `tauri-macros` expands a command without `async` through `body_blocking`,
    /// which calls it inline on the thread the invoke handler arrives on. These
    /// coercions fail to compile the moment one of them loses its `async`, and
    /// that compile failure is the point: the freeze it prevents is a UI
    /// symptom no unit test can observe.
    #[test]
    fn every_command_is_async_and_so_never_runs_on_the_invoke_handlers_thread() {
        macro_rules! answers_a_future {
            ($command:path, $($arg:ident),+) => {{
                fn check<$($arg,)+ R: std::future::Future>(_: fn($($arg),+) -> R) {}
                check($command);
            }};
        }
        answers_a_future!(list_projects, A);
        answers_a_future!(delete_project, A, B);
        answers_a_future!(create_chapter, A, B, C, D, E);
        answers_a_future!(rename_project, A, B, C);
        answers_a_future!(open_chapter, A, B, C, D);
        answers_a_future!(create_project, A, B, C, D, E);
        answers_a_future!(crate::model_workflows::list_workflow_capabilities, A);
        // Six, since `layout` joined the seam.
        answers_a_future!(crate::exporting::export_chapter, A, B, C, D, E, F, G);
        answers_a_future!(crate::exporting::plan_export_chapter, A, B, C, D, E, F);
    }

    /// And `async` alone is not the whole of it: the body has to leave the
    /// runtime's worker threads too, or a multi-second ingest stalls every
    /// other task spawned on it.
    #[test]
    fn a_command_body_runs_on_a_thread_of_its_own() {
        let caller = std::thread::current().id();
        let ran_on = tauri::async_runtime::block_on(blocking(move || {
            Ok::<_, String>(std::thread::current().id())
        }))
        .unwrap();
        assert_ne!(ran_on, caller, "the body ran on the thread that called it");
    }

    /// A panic in a command's body settles the promise as a failure. An
    /// unsettled promise is a spinner that never stops.
    #[test]
    fn a_panicking_body_is_a_failed_call_rather_than_a_promise_that_never_settles() {
        let answer: Result<(), String> =
            tauri::async_runtime::block_on(blocking(|| panic!("the body gave up")));
        assert!(answer.is_err());
    }

    /// `review_state` is the only record a resumed job has of why a region was
    /// flagged, and `src/lib/model/review.js` derives the reason from the
    /// region's fields - so the fields have to come back.
    #[test]
    fn a_flagged_region_comes_back_flagged() {
        let scratch = Scratch::new("review-state");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();

        job.complete_region(
            0,
            &a_patch("r1", Rect::new(0, 0, 8, 8), Engine::Lama),
            Some("review.reason.fittingReconstructed".into()),
        )
        .unwrap();
        job.complete_region(
            0,
            &a_patch("r2", Rect::new(0, 0, 8, 8), Engine::Lama),
            Some("review.reason.unusuallyLarge".into()),
        )
        .unwrap();
        job.complete_region(
            0,
            &a_patch("r3", Rect::new(0, 0, 8, 8), Engine::Lama),
            Some("review.reason.cloudRejectedResidualTest".into()),
        )
        .unwrap();
        job.complete_region(
            0,
            &a_patch("r4", Rect::new(0, 0, 8, 8), Engine::Cloud),
            Some("review.reason.cloudAccepted".into()),
        )
        .unwrap();
        job.complete_region(
            0,
            &a_patch("r5", Rect::new(0, 0, 8, 8), Engine::Flux),
            Some("review.reason.checkGeneratedTexture".into()),
        )
        .unwrap();

        let pages = library.load_pages(&chapter.id, &[0]).unwrap();
        let regions = &pages[0].regions;
        assert!(regions[0].mask.as_ref().unwrap().fitting_reconstructed);
        assert!(regions[1].unusually_large);
        assert_eq!(regions[1].mask.as_ref().unwrap().fill_mode, "reconstruct");

        let rejected = regions[2]
            .mask
            .as_ref()
            .unwrap()
            .cloud_outcome
            .as_ref()
            .unwrap();
        assert!(!rejected.accepted);
        assert_eq!(rejected.rejection_cause, Some("residual-test"));

        // The legacy accepted flag comes back as nothing at all: it diagnosed
        // nothing, and a cloud layer is not a problem for using the cloud.
        let accepted = regions[3].mask.as_ref().unwrap();
        assert!(accepted.cloud_outcome.is_none());
        assert_eq!(review_reason(&regions[3]), None);
        assert!(regions[4].mask.as_ref().unwrap().generated_texture_review);
        assert_eq!(review_reason(&regions[4]), Some("review.reason.checkGeneratedTexture"));
        assert_eq!(pages[0].review_count, 4);
    }

    /// The three readings of "what needs review" on one chapter - a page
    /// header's counts, a resident page's own regions, and the chapter-wide
    /// review index the ⌃ ⌄ pill steps - as `(region, done, review,
    /// candidate)` per page and `(page, reason)` per index entry.
    type Readings = (Vec<(u32, u32, u32, u32)>, Vec<(u32, u32, u32, u32)>, Vec<(u32, &'static str)>);

    fn readings(library: &Library, project_id: &str, chapter_id: &str, pages: usize) -> Readings {
        let opened = library.open_chapter(project_id, chapter_id, false).unwrap().unwrap();
        let counts = |page: &ApiPage| (page.region_count, page.done_count, page.review_count, page.candidate_count);
        let headers = opened.chapter.pages.iter().map(counts).collect();
        let indices: Vec<usize> = (0..pages).collect();
        let resident: Vec<_> = library.load_pages(chapter_id, &indices).unwrap();
        // Resident counts come off the regions the Layers panel lists.
        let listed = resident.iter().map(|page| {
            let flagged = page.regions.iter().filter(|region| review_reason(region).is_some()).count() as u32;
            let held = page.regions.iter().filter(|region| region.outcome == "candidate").count() as u32;
            assert_eq!((page.review_count, page.candidate_count), (flagged, held), "a page's counts are its regions'");
            counts(page)
        }).collect();
        let index = opened.chapter.review.iter().map(|entry| (entry.page_index, entry.reason_key)).collect();
        (headers, listed, index)
    }

    /// **Candidates are not failures.** The grouping holds lettering no text
    /// box claimed as an untouched row under its own reason. It used to fall
    /// through to "declined" and read "every engine failed the quality check"
    /// in Layers, and to count as a page problem; it is a candidate now,
    /// awaiting a choice, in no review count. A text box with no lettering
    /// under it is still flagged, for its mask, and a real decline still is.
    #[test]
    fn held_candidates_await_a_choice_and_are_not_failures() {
        let scratch = Scratch::new("review-candidates");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None).unwrap().created().unwrap();
        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        job.complete_region(0, &a_patch("cleaned", Rect::new(0, 0, 8, 8), Engine::Fill), None).unwrap();
        // One from before the balloon was recorded, one held by this build.
        job.leave_untouched(0, Rect::new(2, 2, 4, 4), "review.reason.unassignedMask").unwrap();
        job.hold_candidate(0, Rect::new(3, 3, 4, 4), "review.reason.isolatedMask", true).unwrap();
        job.leave_untouched(0, Rect::new(4, 4, 4, 4), "review.reason.maskMissingUnderBox").unwrap();
        job.leave_untouched(0, Rect::new(5, 5, 4, 4), "decline.reason.qualityMetric").unwrap();
        drop(job);

        let page = &library.load_pages(&chapter.id, &[0]).unwrap()[0];
        let by_box = |x: i64| page.regions.iter()
            .find(|region| region.id.ends_with(&format!("-u{x}-{x}-4-4"))).unwrap();
        for (x, key) in [(2, "review.reason.unassignedMask"), (3, "review.reason.isolatedMask")] {
            let candidate = by_box(x);
            assert_eq!(candidate.outcome, "candidate");
            assert_eq!(candidate.candidate_reason.as_deref(), Some(key));
            assert_eq!(candidate.decline_reason, None);
            assert_eq!(review_reason(candidate), None, "a candidate is not a problem");
        }
        assert_eq!((by_box(2).candidate_inside_bubble, by_box(3).candidate_inside_bubble), (None, Some(true)),
            "the balloon a candidate was held in reaches the seam; an older row says nothing");
        assert_eq!(by_box(4).candidate_inside_bubble, None);
        assert_eq!(review_reason(by_box(4)), Some("review.reason.maskMissingUnderBox"));
        assert_eq!(review_reason(by_box(5)), Some("review.reason.declined"));
        assert_eq!(
            (page.region_count, page.done_count, page.review_count, page.candidate_count),
            (5, 1, 2, 2),
        );

        let before = readings(&library, &project.id, &chapter.id, 1);
        assert_eq!(before.0, before.1, "header and resident page agree");
        assert_eq!(before.2, vec![(0, "review.reason.maskMissingUnderBox"), (0, "review.reason.declined")]);
        // Reopened from disk by a library that has never seen it.
        invalidate_manifest_cache(&library.resolve_chapter(&chapter.id).unwrap());
        assert_eq!(readings(&Library::at(scratch.join("library")), &project.id, &chapter.id, 1), before);
    }

    /// The grouping keys a cleaned text group carries are reasons with a
    /// meaning, and a stored key this build has no meaning for stays flagged
    /// rather than being cleared by being ignored.
    #[test]
    fn grouping_reasons_on_a_cleaned_layer_are_named_and_unknown_ones_stay_flagged() {
        let scratch = Scratch::new("review-grouping-keys");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None).unwrap().created().unwrap();
        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        for (id, key) in [
            ("crossing", Some("review.reason.crossesBalloon")),
            ("otsu", Some("review.reason.maskMissingUnderBox")),
            // Stored by a newer build: a key this catalogue has no entry for.
            ("future", Some("grouping.newerReason")),
            ("chosen", Some("review.reason.unassignedMask")),
            ("plain", None),
        ] {
            job.complete_region(0, &a_patch(id, Rect::new(0, 0, 8, 8), Engine::Cloud), key.map(str::to_owned)).unwrap();
        }
        drop(job);
        let page = &library.load_pages(&chapter.id, &[0]).unwrap()[0];
        let reason = |id: &str| review_reason(page.regions.iter().find(|region| region.id == id).unwrap());
        assert_eq!(reason("crossing"), Some("review.reason.crossesBalloon"));
        assert_eq!(reason("otsu"), Some("review.reason.maskMissingUnderBox"));
        assert_eq!(reason("future"), Some("review.reason.unrecognized"));
        assert_eq!(reason("chosen"), None, "a candidate somebody chose to clean is answered");
        assert_eq!(reason("plain"), None, "a cloud layer is not flagged for using the cloud");
        assert_eq!((page.done_count, page.review_count), (2, 3));
    }

    /// **A synthetic job from before this build**, with every legacy review
    /// state an older build could have stored, read through the load path. The
    /// known ones mean what they meant, the accepted-cloud flag is migrated
    /// away, an input change stays, and nothing is rewritten on disk.
    #[test]
    fn a_legacy_job_keeps_its_meaningful_flags_and_loses_only_the_cloud_one() {
        let scratch = Scratch::new("review-legacy-fixture");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library.create_chapter(&project.id, "Ch 1", None, None).unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let mut job = Job::open(&path).unwrap();
        let states = [
            ("accepted", "review.reason.cloudAccepted"),
            ("rejected", "review.reason.cloudRejectedTransportError"),
            ("fit", "review.reason.fittingReconstructed"),
            ("large", "review.reason.unusuallyLarge"),
            ("changed", "review.reason.inputChanged"),
            ("unknown", "review.reason.inputUnknown"),
        ];
        for (id, _) in states {
            job.complete_region(0, &a_patch(id, Rect::new(0, 0, 8, 8), Engine::Cloud), None).unwrap();
        }
        job.leave_untouched(0, Rect::new(1, 1, 4, 4), "review.reason.gateSkippedOutsideBubble").unwrap();
        job.leave_untouched(0, Rect::new(2, 2, 4, 4), "decline.reason.tooLarge").unwrap();
        drop(job);
        // Written as an older build wrote it: a version-3 manifest whose rows
        // carry the keys as plain strings.
        let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["version"] = serde_json::json!(3);
        for (row, (id, key)) in raw["patches"].as_array_mut().unwrap().iter_mut().zip(states) {
            assert_eq!(row["id"], id);
            row["review_state"] = serde_json::json!(key);
        }
        let bytes = serde_json::to_vec_pretty(&raw).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        invalidate_manifest_cache(&path);

        let page = &library.load_pages(&chapter.id, &[0]).unwrap()[0];
        let reason = |id: &str| review_reason(page.regions.iter().find(|region| region.id == id).unwrap());
        assert_eq!(reason("accepted"), None);
        assert_eq!(reason("rejected"), Some("review.reason.cloudRejectedTransportError"));
        assert_eq!(reason("fit"), Some("review.reason.fittingReconstructed"));
        assert_eq!(reason("large"), Some("review.reason.unusuallyLarge"));
        assert_eq!(reason("changed"), Some("review.reason.inputChanged"));
        assert_eq!(reason("unknown"), Some("review.reason.inputUnknown"));
        let held: Vec<_> = page.regions.iter().filter(|region| region.mask.is_none())
            .map(|region| (region.outcome, review_reason(region))).collect();
        assert_eq!(held, vec![
            ("gate-skipped", Some("review.reason.gateSkippedOutsideBubble")),
            ("declined", Some("review.reason.declined")),
        ]);
        assert_eq!((page.region_count, page.done_count, page.review_count, page.candidate_count), (8, 1, 7, 0));
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "reading migrates nothing on disk");
    }

    /// **A copy of a real chapter**, when one is supplied: run with
    /// `MC_LEGACY_MANIFEST=<a copy of a .mtclean> cargo test -p manga-cleaner
    /// real_legacy_manifest -- --ignored --nocapture`. The file is copied again
    /// into scratch before it is opened, so the path given is never opened for
    /// writing. It checks that the header counts, the resident counts and the
    /// review index agree over every page, and that no successful clean is
    /// flagged for anything but a stored reason that means one.
    #[test]
    #[ignore]
    fn real_legacy_manifest_counts_agree() {
        let Some(given) = std::env::var_os("MC_LEGACY_MANIFEST") else { return };
        let scratch = Scratch::new("review-real-manifest");
        let path = scratch.join("chapter.mtclean");
        std::fs::copy(&given, &path).unwrap();
        let job = Job::open(&path).unwrap();
        let project = &job.project;
        let pages = project.strip.order.len();
        let all: Vec<usize> = (0..pages).collect();
        let (resident, index) = pages_and_review("c", project, &all);
        let (headers, header_index) = pages_and_review("c", project, &[]);
        assert_eq!(index, header_index);
        let totals = |pages: &[ApiPage]| pages.iter().fold((0, 0, 0, 0), |sum, page| (
            sum.0 + page.region_count, sum.1 + page.done_count, sum.2 + page.review_count, sum.3 + page.candidate_count,
        ));
        assert_eq!(totals(&resident), totals(&headers));
        let mut reasons = std::collections::BTreeMap::<&str, u32>::new();
        for entry in &index {
            *reasons.entry(entry.reason_key).or_default() += 1;
        }
        for page in &resident {
            assert_eq!(page.review_count as usize, index.iter().filter(|entry| entry.page_id == page.id).count());
        }
        // Every flag on a cleaned layer is one of its stored reasons.
        let stored = project.patches.iter().filter(|row| row.visible)
            .filter(|row| review_flags(row.review_state.as_deref()) != ReviewFlags::default()).count() as u32;
        let flagged_layers = resident.iter().flat_map(|page| &page.regions)
            .filter(|region| region.mask.is_some() && region.outcome == "cleaned" && review_reason(region).is_some())
            .count() as u32;
        assert!(flagged_layers <= stored + project.text_shape_corrections.len() as u32);
        println!("pages={pages} totals(region, done, review, candidate)={:?} reasons={reasons:?} stored-flagged-layers={stored}",
            totals(&resident));
    }

    #[test]
    fn an_interrupted_job_reaches_the_project_that_holds_it() {
        let scratch = Scratch::new("interrupted");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 4);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        assert!(library.list_projects().unwrap()[0]
            .interrupted_job
            .is_none());

        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        job.mark_interrupted(Some(2)).unwrap();

        let interrupted = library.list_projects().unwrap()[0]
            .interrupted_job
            .clone()
            .unwrap();
        assert_eq!(interrupted.chapter_id, chapter.id);
        assert_eq!(interrupted.page_index, 2);
    }

    /* ---------- the input report ---------- */

    #[test]
    fn refused_files_are_not_pages_and_are_reported_as_notices() {
        let scratch = Scratch::new("skipped");
        let library = library(&scratch);
        let root = scratch.join("raws");
        scans(&root, 2);
        std::fs::write(root.join("003.png"), b"not an image").unwrap();
        std::fs::write(root.join(".DS_Store"), b"junk").unwrap();
        let project = library
            .create_project("WM", StripMode::Single, Some(root), None)
            .unwrap();
        library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created();

        let chapter = library.list_projects().unwrap()[0].chapters[0].clone();
        assert_eq!(chapter.pages.len(), 2, "a refused file took a page index");
        let keys: Vec<&str> = chapter
            .input_reports
            .iter()
            .map(|n| n.key.as_str())
            .collect();
        assert!(keys.contains(&"notice.input.junkSkipped"));
        assert!(keys.contains(&"notice.input.fileSkipped"));
        let skipped = chapter
            .input_reports
            .iter()
            .find(|notice| notice.key == "notice.input.fileSkipped")
            .unwrap();
        assert_eq!(skipped.params["file"], "003.png");
        assert_eq!(skipped.params["reasonKey"], "input.skipReason.notAnImage");
    }

    /// *Skipped and listed*, never
    /// silent - and **listed under the reason it was actually refused for**.
    ///
    /// One folder holding one of everything ingest can refuse, and the notices
    /// the chapter carries have to name each reason exactly once, with a count
    /// that is true of that reason alone. Reporting the first refusal with the
    /// total count said "four files skipped because one of them is not an
    /// image", which is a false sentence about three files.
    ///
    /// Two of the catalogue's six keys are not here, and their absence is the
    /// finding rather than an omission in the test. `input.skipReason.junk` is
    /// never a `reasonKey` on the seam: junk is filtered out of the manifest's
    /// `skipped` list by `InputReport::of` and reported as a count under
    /// `notice.input.junkSkipped`, which is the notice the copy was written
    /// for. `input.skipReason.unsupportedMode` cannot be produced by an ingest
    /// at all - `decode_whole` maps `ImageError::Unrepresentable` to it, and
    /// that error is raised only by the two *encoders*.
    #[test]
    fn every_refusal_a_folder_can_produce_reaches_the_seam_under_its_own_reason() {
        let scratch = Scratch::new("refusal-keys");
        let library = library(&scratch);
        let root = scans(&scratch.join("raws"), 2);

        // Not an image: no magic this build recognises.
        std::fs::write(root.join("003-notes.txt"), b"a note to self").unwrap();
        // Header unreadable: the magic is a claim, and the rest is not a PNG.
        let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
        broken.extend_from_slice(b"and then nothing that parses");
        std::fs::write(root.join("004-broken.png"), broken).unwrap();
        // Complete chunk framing keeps metadata/header validation meaningful;
        // corrupt compressed image data fails only when pixels are decoded.
        let whole = std::fs::read(root.join("001.png")).unwrap();
        let mut damaged_pixels = whole.clone();
        let mut at = 8;
        while &damaged_pixels[at + 4..at + 8] != b"IDAT" {
            at += 12 + u32::from_be_bytes(damaged_pixels[at..at + 4].try_into().unwrap()) as usize;
        }
        damaged_pixels[at + 8] ^= 0xff;
        std::fs::write(root.join("005-corrupt-pixels.png"), damaged_pixels).unwrap();
        // Duplicate: byte-identical to a file already accepted.
        std::fs::write(root.join("006-again.png"), &whole).unwrap();
        // Junk: counted, never named.
        std::fs::write(root.join(".DS_Store"), b"junk").unwrap();

        let project = library
            .create_project("Refusals", StripMode::Single, Some(root), None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        assert_eq!(chapter.pages.len(), 2, "a refused file took a page index");

        let reasons: Vec<String> = chapter
            .input_reports
            .iter()
            .filter(|notice| notice.key == "notice.input.fileSkipped")
            .map(|notice| {
                notice.params["reasonKey"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        for expected in [
            "input.skipReason.notAnImage",
            "input.skipReason.headerUnreadable",
            "input.skipReason.partialDecode",
            "input.skipReason.duplicate",
        ] {
            assert_eq!(
                reasons.iter().filter(|reason| *reason == expected).count(),
                1,
                "{expected} is not on the stack exactly once: {reasons:?}"
            );
        }
        // Every count is a count of files refused for *that* reason.
        for notice in &chapter.input_reports {
            if notice.key == "notice.input.fileSkipped" {
                assert_eq!(notice.params["count"], 1, "{:?}", notice.params);
            }
        }
        let junk = chapter
            .input_reports
            .iter()
            .find(|notice| notice.key == "notice.input.junkSkipped")
            .expect("junk is reported as a count");
        assert_eq!(junk.params["count"], 1);
    }

    /* ---------- opening ---------- */

    #[test]
    fn opening_a_chapter_returns_both_halves_and_records_the_visit() {
        let scratch = Scratch::new("open");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 2);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let opened = library
            .open_chapter(&project.id, &chapter.id, false)
            .unwrap()
            .unwrap();
        assert_eq!(opened.project.id, project.id);
        assert_eq!(opened.chapter.id, chapter.id);
        assert_eq!(opened.chapter.pages.len(), 2);
        assert!(opened.pending_conversion.is_none());
        assert_eq!(opened.project.chapters.len(), 1);
    }

    /* ---------- the seam's own vocabulary ---------- */

    /// The interface reads these names, not Rust's. A snake_case field here is
    /// a field the interface silently does not find.
    #[test]
    fn the_tree_is_serialised_in_the_names_the_seam_declares() {
        let scratch = Scratch::new("json");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 1);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        let chapter_id = chapter.id.clone();
        let mut job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        job.complete_region(0, &a_patch("r1", Rect::new(0, 0, 8, 8), Engine::Fill), None)
            .unwrap();

        let listed = library.list_projects().unwrap();
        let value = serde_json::to_value(&listed).unwrap();
        let project = &value[0];
        for key in [
            "id",
            "name",
            "mode",
            "readingDirection",
            "created",
            "appVersion",
            "sourcePath",
            "lastOpened",
            "starred",
            "interruptedJob",
            "conversion",
            "chapters",
        ] {
            assert!(project.get(key).is_some(), "ApiProject has no {key}");
        }
        let chapter = &project["chapters"][0];
        for key in [
            "id",
            "projectId",
            "name",
            "number",
            "order",
            "lastOpened",
            "sourcePath",
            "sourceFormat",
            "noTextDetected",
            "inputReports",
            "pages",
            "review",
            "denoiseReplacement",
            "denoiseHistory",
        ] {
            assert!(chapter.get(key).is_some(), "ApiChapter has no {key}");
        }
        let page = &chapter["pages"][0];
        for key in [
            "id",
            "chapterId",
            "index",
            "number",
            "file",
            "sourceSha",
            "width",
            "height",
            "status",
            "skipReason",
            "regions",
            "regionCount",
            "doneCount",
            "reviewCount",
            "resident",
        ] {
            assert!(page.get(key).is_some(), "ApiPage has no {key}");
        }
        // The stand-in canvas fields a real backend does not send.
        for key in ["layout", "panels"] {
            assert!(
                page.get(key).is_none(),
                "ApiPage still carries the mock's {key}"
            );
        }
        // A listing is headers only, so the region shape is checked against a
        // page that has actually been paged in.
        let loaded = serde_json::to_value(library.load_pages(&chapter_id, &[0]).unwrap()).unwrap();
        let region = &loaded[0]["regions"][0];
        for key in [
            "id",
            "pageId",
            "sourceSha",
            "bbox",
            "source",
            "detected",
            "outcome",
            "gateSkipCause",
            "declineReason",
            "unusuallyLarge",
            "mask",
        ] {
            assert!(region.get(key).is_some(), "ApiRegion has no {key}");
        }
        for key in ["kind", "text"] {
            assert!(
                region.get(key).is_none(),
                "ApiRegion still carries the mock's {key}"
            );
        }
        let mask = &region["mask"];
        for key in [
            "id",
            "regionId",
            "sequence",
            "fillMode",
            "elapsedMs",
            "fittingReconstructed",
            "cloudOutcome",
            "provenance",
        ] {
            assert!(mask.get(key).is_some(), "Mask has no {key}");
        }
        // Provenance keeps its field names verbatim - snake_case, the one
        // island in a camelCase payload, because that record round-trips.
        for key in [
            "engine",
            "engine_version",
            "model_sha256",
            "execution_provider",
            "params_snapshot",
            "mask_sha256",
            "source_sha256",
            "cloud",
            "created",
        ] {
            assert!(
                mask["provenance"].get(key).is_some(),
                "Provenance has no {key}"
            );
        }
        assert_eq!(project["mode"], "single");
        assert_eq!(project["readingDirection"], "rtl");
    }

    /* ---------- time ---------- */

    #[test]
    fn epoch_seconds_become_iso_8601_in_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(iso8601(1_760_000_000), "2025-10-09T08:53:20Z");
        // A leap day, which is where a hand-rolled calendar goes wrong.
        assert_eq!(iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn a_timestamp_becomes_one_of_the_catalogues_buckets() {
        let day = 86_400;
        let cases = [
            (0, "time.relative.justNow"),
            (2 * 3_600, "time.relative.hoursAgo"),
            (30 * 3_600, "time.relative.yesterday"),
            (3 * day, "time.relative.daysAgo"),
            (8 * day, "time.relative.lastWeek"),
            (30 * day, "time.relative.weeksAgo"),
        ];
        let now = 10_000_000;
        for (elapsed, key) in cases {
            assert_eq!(relative_time(now, now - elapsed).key, key, "at {elapsed}s");
        }
        assert_eq!(relative_time(now, now - 2 * 3_600).params["count"], 2);
        // A clock that went backwards is "just now", not a panic on subtraction.
        assert_eq!(relative_time(0, 1_000).key, "time.relative.justNow");
    }

    #[test]
    fn a_zero_sized_page_has_no_percentage_rather_than_a_division() {
        assert_eq!(
            percent_of(Rect::new(4, 4, 8, 8), 0, 0),
            Bbox {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0
            }
        );
    }

    #[test]
    fn consecutive_load_pages_calls_parse_manifest_once_and_writes_invalidate() {
        let scratch = Scratch::new("manifest-cache");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 4);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();
        let chapter_id = chapter.id.clone();
        let path = library.resolve_chapter(&chapter_id).unwrap();

        invalidate_manifest_cache(&path);
        reset_parse_count(&path);

        // 1. First load_pages call parses from disk.
        let pages1 = library.load_pages(&chapter_id, &[0, 1]).unwrap();
        assert_eq!(pages1.len(), 2);
        assert_eq!(
            parse_count(&path),
            1,
            "first load_pages should parse manifest once"
        );

        // 2. Second consecutive load_pages call hits cache and does not re-parse.
        let pages2 = library.load_pages(&chapter_id, &[1, 2]).unwrap();
        assert_eq!(pages2.len(), 2);
        assert_eq!(
            parse_count(&path),
            1,
            "second load_pages should reuse cached manifest"
        );

        // 3. A write occurs (e.g. adding a patch and flushing).
        let mut job = Job::open(&path).unwrap();
        job.complete_region(0, &a_patch("r1", Rect::new(0, 0, 8, 8), Engine::Fill), None)
            .unwrap();
        drop(job);

        // 4. Next load_pages detects change and parses the updated manifest from disk.
        let pages3 = library.load_pages(&chapter_id, &[0]).unwrap();
        assert_eq!(pages3.len(), 1);
        assert_eq!(pages3[0].regions.len(), 1);
        assert_eq!(
            parse_count(&path),
            2,
            "load_pages after write should re-parse modified manifest"
        );

        // 5. Subsequent load_pages again uses cache.
        let pages4 = library.load_pages(&chapter_id, &[0]).unwrap();
        assert_eq!(pages4.len(), 1);
        assert_eq!(
            parse_count(&path),
            2,
            "subsequent load_pages should reuse cached manifest"
        );
    }

    #[test]
    fn concurrent_chapter_creations_and_opens_do_not_lose_updates() {
        let scratch = Scratch::new("concurrent-index-lock");
        let library = Arc::new(library(&scratch));
        let project = library
            .create_project("Concurrent", StripMode::Single, None, None)
            .unwrap();
        let project_id = project.id.clone();

        let mut handles = Vec::new();
        for i in 0..8 {
            let lib = Arc::clone(&library);
            let pid = project_id.clone();
            handles.push(std::thread::spawn(move || {
                let ch_name = format!("Chapter {i}");
                lib.create_chapter(&pid, &ch_name, None, None).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let index = library.index().unwrap();
        let proj = index.projects.iter().find(|p| p.id == project_id).unwrap();
        assert_eq!(
            proj.chapters.len(),
            8,
            "all concurrent chapters must be preserved in index"
        );
    }

    #[test]
    fn removing_an_untouched_region_decrements_manifest_counters() {
        let scratch = Scratch::new("remove-untouched-counters");
        let library = library(&scratch);
        let project = a_project(&library, &scratch, 2);
        let chapter = library
            .create_chapter(&project.id, "Ch 1", None, None)
            .unwrap()
            .created()
            .unwrap();

        let path = library.resolve_chapter(&chapter.id).unwrap();
        let mut job = Job::open(&path).unwrap();
        job.leave_untouched(
            0,
            Rect::new(4, 6, 10, 10),
            "review.reason.gateSkippedNotJapanese",
        )
        .unwrap();
        job.leave_untouched(1, Rect::new(2, 2, 8, 8), "decline.reason.qualityMetric")
            .unwrap();
        job.project.counters.gate_dropped = 1;
        job.project.counters.declined = 1;
        job.flush().unwrap();
        drop(job);

        let pages = library.load_pages(&chapter.id, &[0, 1]).unwrap();
        let gated_id = pages[0].regions[0].id.clone();
        let declined_id = pages[1].regions[0].id.clone();

        // Check counters before removal
        let job_before = Job::open(&path).unwrap();
        assert_eq!(job_before.project.counters.gate_dropped, 1);
        assert_eq!(job_before.project.counters.declined, 1);
        drop(job_before);

        // Remove the gate-skipped region
        library.remove_region(&gated_id).unwrap();
        let job_after_gated = Job::open(&path).unwrap();
        assert_eq!(job_after_gated.project.counters.gate_dropped, 0);
        assert_eq!(job_after_gated.project.counters.declined, 1);
        drop(job_after_gated);

        // Remove the declined region
        library.remove_region(&declined_id).unwrap();
        let job_after_declined = Job::open(&path).unwrap();
        assert_eq!(job_after_declined.project.counters.gate_dropped, 0);
        assert_eq!(job_after_declined.project.counters.declined, 0);
    }

    #[test]
    fn sanitize_identifier_accepts_valid_and_rejects_unsafe() {
        // Valid identifiers including namespace/name model IDs
        assert_eq!(
            sanitize_identifier("beam-us-east-1"),
            Some("beam-us-east-1".to_string())
        );
        assert_eq!(
            sanitize_identifier("flux-sdnq-klein-v1"),
            Some("flux-sdnq-klein-v1".to_string())
        );
        assert_eq!(
            sanitize_identifier("black-forest-labs/FLUX.1-schnell"),
            Some("black-forest-labs/FLUX.1-schnell".to_string())
        );
        assert_eq!(
            sanitize_identifier("req_123_abc.v2"),
            Some("req_123_abc.v2".to_string())
        );

        // Disallowed slashes (leading, trailing, consecutive)
        assert_eq!(sanitize_identifier("/model"), None);
        assert_eq!(sanitize_identifier("model/"), None);
        assert_eq!(sanitize_identifier("org//model"), None);

        // Control characters & whitespace
        assert_eq!(sanitize_identifier("bad\nidentifier"), None);
        assert_eq!(sanitize_identifier("bad\tidentifier"), None);
        assert_eq!(sanitize_identifier("bad\0identifier"), None);
        assert_eq!(sanitize_identifier("bad identifier"), None);

        // URLs & userinfo (contains ':', '@', etc. not in allowlist)
        assert_eq!(sanitize_identifier("https://api.beam.cloud/v1"), None);
        assert_eq!(sanitize_identifier("http://modal.com/endpoint"), None);
        assert_eq!(sanitize_identifier("//evil.com/leak"), None);
        assert_eq!(sanitize_identifier("user:password@host"), None);
        assert_eq!(sanitize_identifier("foo@bar"), None);
        assert_eq!(sanitize_identifier("key=secret"), None);
        assert_eq!(sanitize_identifier("val$name"), None);

        // Bounds: > 128 characters
        let long_str = "a".repeat(129);
        assert_eq!(sanitize_identifier(&long_str), None);
        let max_str = "a".repeat(128);
        assert_eq!(sanitize_identifier(&max_str), Some(max_str));

        // Empty or whitespace
        assert_eq!(sanitize_identifier(""), None);
        assert_eq!(sanitize_identifier("   "), None);
    }

    #[test]
    fn api_provenance_of_sanitizes_cloud_record_and_preserves_clean_data() {
        let dirty_cloud = CloudRecord {
            provider: "beam".to_string(),
            profile_id: Some("https://evil.com/token".to_string()),
            job_id: Some("Bearer secret_jwt".to_string()),
            request_id: "req-clean-123".to_string(),
            attempt_id: Some("attempt\nwith\nnewlines".to_string()),
            recipe_id: Some("flux-sdnq-v1".to_string()),
            model: "https://evil.com/malicious-model".to_string(),
            model_revision: Some("a".repeat(130)),
            tier: Some("gpu-t4".to_string()),
            cost: None,
            duration_ms: Some(1500),
        };

        let prov = cleaner_core::patch::Provenance {
            engine: Engine::Flux,
            engine_version: "flux-sdnq-v1".to_string(),
            model_sha256: None,
            execution_provider: "beam".to_string(),
            params_snapshot: serde_json::json!({}),
            mask_sha256: "m123".to_string(),
            source_sha256: "s123".to_string(),
            cloud: Some(dirty_cloud.clone()),
            created: 1700000000,
        };

        let api_prov = ApiProvenance::of(&prov);
        let api_cloud = api_prov.cloud.expect("cloud record present");
        assert_eq!(api_cloud.provider, "beam");
        assert_eq!(api_cloud.request_id, "req-clean-123");
        assert_eq!(api_cloud.recipe_id.as_deref(), Some("flux-sdnq-v1"));
        assert_eq!(api_cloud.tier.as_deref(), Some("gpu-t4"));
        assert_eq!(api_cloud.cost, None);
        assert_eq!(api_cloud.duration_ms, Some(1500));

        // Unsafe fields rejected and stripped/fallen back
        assert_eq!(api_cloud.profile_id, None, "URL in profile_id stripped");
        assert_eq!(api_cloud.job_id, None, "Bearer token in job_id stripped");
        assert_eq!(
            api_cloud.attempt_id, None,
            "Control chars in attempt_id stripped"
        );
        assert_eq!(
            api_cloud.model, "unknown",
            "Unsafe URL in model falls back to unknown"
        );
        assert_eq!(
            api_cloud.model_revision, None,
            "Overly long string stripped"
        );

        // The core provenance in memory / on disk was untouched
        assert_eq!(prov.cloud, Some(dirty_cloud));
    }
}
