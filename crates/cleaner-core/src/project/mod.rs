//! The `.mtclean` project file, which is also the job manifest.
//!
//! Three sentences shape everything here:
//!
//! > **This file is the job manifest** flushed after every completed region -
//! > one artefact, not two. It is rewritten atomically via temp-and-rename.
//!
//! > On launch, any incomplete job offers a resume, and resume re-verifies
//! > source hashes before continuing.
//!
//! > **Output never overwrites input.**
//!
//! The first is why [`Job`] exists rather than a pair of free functions: a
//! caller that has to remember to flush is a caller that will not, and the
//! failure mode is a crash losing completed regions - the exact thing §6 is
//! written to prevent. So the only way to record a region is a method that
//! writes the sidecar buffers, appends the record and rewrites the manifest,
//! in that order.
//!
//! **One writer per job, across processes.** Every write goes through the
//! job's lock ([`lock`]), which excludes this process's other threads and every
//! other process over the same library, and every save refuses to replace a
//! manifest that is no longer the one this job read ([`StoreError::Stale`]).
//! Two copies of the application, or the application and a headless tool, can
//! no longer lose each other's committed regions.
//!
//! What is **not** here, and belongs to Phase 6: the LRU
//! patch cache, the free-disk preflight and the orphaned-session sweep.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::image::{BitDepth, ColorMode};
use crate::ingest::{sha256_hex, IngestReport, SourceRef, Warning};
use crate::mask::Rect;
use crate::patch::{Engine, Patch, Provenance};
use crate::text_shape::{
    GeometryPolicy, MaskPlan, MaskQualityState, MaskRaster, PlanIdentity, PreparedMaskPlan,
};
use sha2::{Digest, Sha256};

pub mod buffers;
pub mod lock;
pub mod orientation;

/// The manifest format this build writes. A file from a later version is
/// refused rather than half-read: a manifest is the only record that a job's
/// completed regions exist.
///
/// **What this number can and cannot protect.** [`Job::open`] reads it *before*
/// deserialising the rest, so a version it does not recognise is refused as
/// [`StoreError::Version`] whatever shape the body has. That ordering is the
/// whole value of the field: with the version read out of the fully
/// deserialised [`Project`], a future version whose shape had also changed
/// would fail as [`StoreError::Json`] and the version gate would never be
/// reached - the check would fire only for the files that did not need it.
///
/// It still says nothing about a shape that changes *without* the number
/// changing. That is a decision to be justified in each case, and
/// [`Strip::splits`] carries the one instance of it in this build.
pub const FORMAT_VERSION: u32 = 5;

/// The oldest manifest this build reads.
///
/// Version 4 adds prepared text-shape mask plans and a per-patch geometry
/// policy. Missing plan rows and missing patch policy fields deserialize as
/// legacy, so v1-v3 projects open without reinterpreting their saved masks.
/// Version 5 adds oriented views and grouped native patch intersections.
/// Existing version-three and version-four jobs retain their schema until needed.
/// Opening upgrades the in-memory version; the disk file remains untouched
/// until the next [`Job::flush`].
pub const OLDEST_READABLE_VERSION: u32 = 1;

/// The extension §1 names, and the sidecar directory beside it.
pub const EXTENSION: &str = "mtclean";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the manifest is not readable as JSON: {0}")]
    Json(String),
    #[error("{0}")]
    Malformed(String),
    #[error("prepared text-shaped mask plan is stale or does not match the approved preview")]
    StalePlan,
    #[error("this build reads manifest version {expected}; the file is version {found}")]
    Version { expected: u32, found: u32 },
    /// The manifest on disk is not the one this job read or last wrote:
    /// another writer saved it in between. The save is refused so that
    /// writer's work stays; the job has to be opened again. The display starts
    /// with the code `job_stale`, which the interface reads wherever the
    /// message ends up and answers by reading the chapter again.
    #[error("job_stale: {} changed on disk after this job read it; the save was refused", path.display())]
    Stale { path: PathBuf },
    /// Another process held the job for longer than a writer waits
    /// ([`lock::PATIENCE`]). Nothing was written. Starts with the code
    /// `job_busy`, as [`lock::LockError::Busy`] does.
    #[error("job_busy: another Manga Cleaner process is using {}", path.display())]
    Busy { path: PathBuf },
    /// A write asked of a job opened with [`Job::open_read_only`]. Nothing
    /// was written, and no lock was taken.
    #[error("job_read_only: {} was opened to read only; nothing was written", path.display())]
    ReadOnly { path: PathBuf },
}

impl From<lock::LockError> for StoreError {
    fn from(error: lock::LockError) -> StoreError {
        match error {
            lock::LockError::Busy { path } | lock::LockError::Stopped { path } => StoreError::Busy { path },
        }
    }
}

impl StoreError {
    fn io(path: &Path, source: std::io::Error) -> StoreError {
        StoreError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// A sidecar writer and an opener's orphan sweep cannot cross in this process:
/// the writer names new files before its manifest flush, and the opener must
/// not call those files orphaned during that interval.
fn detection_io() -> &'static Mutex<()> {
    static IO: OnceLock<Mutex<()>> = OnceLock::new();
    IO.get_or_init(|| Mutex::new(()))
}

/// Single page or longstrip. §1's `strip.mode`, chosen once on the home
/// screen and fixed for the project's life, because it determines stitching and
/// split points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StripMode {
    Single,
    Longstrip,
}

/// §1's `sources` row. The header fields are here rather than only in
/// [`SourceRef`] because §6 re-verifies "sha256, mtime and dimensions" and a
/// resume must be able to do that without decoding anything.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectSource {
    /// Preserve fields written by other compatible builds when saving edits.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// Native geometry stays in w/h; display uses this EXIF permutation.
    #[serde(default, skip_serializing_if = "orientation_is_identity")]
    pub orientation: crate::image::orientation::Orientation,
    /// The page's own file, relative to the manifest's own directory where a
    /// relative path can be formed and absolute where it cannot - a path on
    /// another volume has no relative form. Both resolve through the same
    /// `join`, because joining an absolute path replaces the base.
    ///
    /// For a chapter created by this build this is always a file inside the
    /// job's own sidecar, put there at ingest; the user's file is
    /// `converted_from`. A manifest written before that names the user's file
    /// here and reads unchanged.
    pub rel_path: PathBuf,
    pub sha256: String,
    /// Seconds since the Unix epoch, where the filesystem gave one.
    pub mtime: Option<u64>,
    pub w: u32,
    pub h: u32,
    pub mode: ColorMode,
    pub bit_depth: BitDepth,
    /// The user's own file, which ingest copied or converted to produce
    /// `rel_path` - see [`crate::ingest::SourceRef::converted_from`]. Relative
    /// to the manifest's own directory on the same terms as `rel_path`.
    ///
    /// **This is the origin, and it is not required to still exist.** Nothing
    /// reads it: it is what [`Job::origin_path`] answers "where did this
    /// chapter come from" with, and what [`Job::output_refusal`] refuses to
    /// write over. A user who deletes their scan folder after creating the
    /// chapter loses neither the pages nor either of those answers - an export
    /// whose default directory is gone is a directory the exporter creates.
    ///
    /// Optional and defaulted, so a manifest written before ingest imported
    /// anything reads unchanged and no [`FORMAT_VERSION`] bump is owed: the
    /// field only ever *adds* a second path to a row that already resolved on
    /// its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converted_from: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversion: Option<crate::ingest::ConversionProvenance>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Strip {
    pub mode: StripMode,
    /// Indices into `sources`, in reading order. Held separately from the
    /// sources list because ingest order is natural-sort order and a user may
    /// reorder pages without the sources moving.
    pub order: Vec<usize>,
    /// Split rows in **strip** coordinates, from rule 2. Always empty
    /// for `Single`, where the page boundaries are the splits.
    ///
    /// A row rather than a bare number, because rule 2 requires a split that
    /// found no acceptable row to be "recorded as `split_fallback: true` in
    /// `strip.splits` and surfaced in review" - so the record has to carry more
    /// than the row. [`crate::strip::SplitKind`] carries that bit and two more.
    ///
    /// **The element type changed from `u32` to [`crate::strip::Split`] without
    /// a [`FORMAT_VERSION`] bump, and that is a decision rather than an
    /// oversight.** `#[serde(default)]` covers an *absent* field, not a
    /// populated one: a manifest holding `"splits": [2000]` fails [`Job::open`]
    /// with a decode error.
    ///
    /// No such manifest can exist. [`Project::new`] is the only constructor of
    /// this struct and it writes `Vec::new()`; nothing in this workspace has
    /// ever assigned a non-empty `splits`, because rule 2's planner
    /// ([`crate::strip::plan_splits`]) has no caller - so every `.mtclean` file
    /// any build has written carries `[]`, which decodes identically under both
    /// element types. The change is invisible to every file it could meet.
    ///
    /// Bumping instead would cost more than it bought, in both directions.
    /// Every existing manifest would be refused by [`Job::open`], including the
    /// ones whose `splits` is empty and therefore whose meaning is unchanged;
    /// there is no migration path in this build to read them with, so the
    /// refusal would be permanent rather than a step. And it would not even
    /// improve the error for the file it is nominally about: a v1 file holding
    /// `"splits": [2000]` read by a v2 build is refused for its *version*, but
    /// only because [`Job::open`] now probes the version first - as a shape
    /// mismatch it was always going to be refused either way.
    ///
    /// What this reasoning depends on is that the field has no writer. The
    /// moment rule 2's planner is wired to a manifest, a populated `splits` can
    /// reach disk, and the *next* change to this type is a bump.
    #[serde(default)]
    pub splits: Vec<crate::strip::Split>,
}

fn orientation_is_identity(value: &crate::image::orientation::Orientation) -> bool { value.0 == 1 }

/// §1's `patches` row: one applied edit, and where its pixels live.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PatchRecord {
    /// Preserve fields written by other compatible builds when saving edits.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// Version 5 grouped edits: independently stored native page intersections.
    /// Empty means the legacy anchor-native/global-strip representation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_parts: Vec<NativePart>,
    /// Frozen layout for legacy strip intersections, before any later reorder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_order: Vec<usize>,
    pub id: String,
    pub source_idx: usize,
    pub bbox: Rect,
    /// File names within `<job>.mtclean.d/`.
    pub mask_ref: String,
    pub buffer_ref: String,
    pub engine: Engine,
    pub order: u32,
    pub visible: bool,
    /// The `review.reason.*` key this region was flagged under, or `None` if it
    /// needs no review.
    ///
    /// Stored rather than recomputed because the interface derives it from a
    /// live `Region` - `src/lib/model/review.js` reads `unusuallyLarge`,
    /// `cloudOutcome` and the gate cause - and a resumed job has no live
    /// region until the page is re-detected. §1 persists `order` and `visible`
    /// for the same reason: state the composite depends on that re-running
    /// would not reproduce.
    pub review_state: Option<String>,
    /// Geometry used to derive this patch's applied mask. Missing in v1-v3
    /// manifests and every existing patch means legacy.
    #[serde(default, skip_serializing_if = "is_legacy_geometry_policy")]
    pub geometry_policy: GeometryPolicy,
    /// Exact prepared-plan identity for a text-shaped patch. Kept separate
    /// from the stable region ID so an older undo state can still identify its
    /// own padding/correction revision after a newer preview is prepared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_shape_plan_identity: Option<String>,
    pub provenance: Provenance,
}

fn is_legacy_geometry_policy(policy: &GeometryPolicy) -> bool {
    *policy == GeometryPolicy::Legacy
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NativePart {
    pub source_idx: usize,
    pub bbox: Rect,
    pub mask_ref: String,
    pub ink_ref: String,
    pub buffer_ref: String,
}

impl PatchRecord {
    /// Bounds after the saved layer move/rotation, without loading its pixels.
    pub fn display_bbox(&self) -> Rect {
        crate::patch::LayerStyle::from_snapshot(&self.provenance.params_snapshot).display_bounds(self.bbox)
    }

    pub fn legacy_revision_id(&self) -> Option<String> {
        (self.geometry_policy == GeometryPolicy::Legacy)
            .then(|| revision_from_buffer_ref(&self.buffer_ref))
            .flatten()
    }

    /// Opaque content revision exposed to undo snapshots. Old M3 records can
    /// still be addressed by their plan identity if no content ref exists.
    pub fn text_shape_revision_id(&self) -> Option<String> {
        (self.geometry_policy == GeometryPolicy::TextShape).then(|| {
            revision_from_buffer_ref(&self.buffer_ref)
                .or_else(|| {
                    self.text_shape_plan_identity
                        .as_ref()
                        .map(|identity| format!("legacy:{identity}"))
                })
                .unwrap_or_default()
        })
    }

    /// The row a patch becomes.
    ///
    /// One constructor rather than two, because the row is built in two places -
    /// [`Job::complete_region`], which persists it, and the run, which
    /// reports the same region on the event channel - and a second spelling is
    /// a `mask_ref` that agrees today and diverges the moment either moves.
    pub fn of(source_idx: usize, patch: &Patch, review_state: Option<String>) -> PatchRecord {
        PatchRecord {
            extra: Default::default(),
            native_parts: Vec::new(),
            native_order: Vec::new(),
            id: patch.id.clone(),
            source_idx,
            bbox: patch.mask.bounds,
            mask_ref: format!("{}.mask", patch.id),
            buffer_ref: format!("{}.buf", patch.id),
            engine: patch.provenance.engine,
            order: patch.order,
            visible: patch.visible,
            review_state,
            geometry_policy: GeometryPolicy::Legacy,
            text_shape_plan_identity: None,
            provenance: patch.provenance.clone(),
        }
    }

    /// The sidecar file holding [`Patch::ink`]: `<id>.ink`, beside the
    /// `<id>.mask` that `mask_ref` names.
    ///
    /// Derived rather than stored, because it is the one sidecar entry that
    /// arrived after the manifest's row was specified, and a manifest without
    /// it still has to read: [`Job::load_patch`] answers a missing file with
    /// the applied mask.
    pub fn ink_ref(&self) -> String {
        if revision_from_buffer_ref(&self.buffer_ref).is_some() {
            self.mask_ref
                .strip_suffix(".mask")
                .map(|stem| format!("{stem}.ink"))
                .unwrap_or_else(|| format!("{}.ink", self.id))
        } else {
            format!("{}.ink", self.id)
        }
    }
}

/// Write a text-group evidence file under `evidence/`, once: the name is the
/// content's digest, so an existing file is already these bytes. Written
/// before the manifest that names it, like every sidecar.
fn write_evidence(dir: &Path, evidence: &crate::text_groups::EvidenceBlob) -> Result<(), StoreError> {
    let path = dir.join(evidence.reference());
    if path.is_file() {
        return Ok(());
    }
    let parent = dir.join("evidence");
    std::fs::create_dir_all(&parent).map_err(|error| StoreError::io(&parent, error))?;
    buffers::write_atomic(&path, &evidence.bytes)
}

/// Whether at least half of `bbox`'s area lies inside `other`: the test for
/// "this region was cleaned already" ([`Job::detection_is_cleaned_already`],
/// and a one-pass run over a page that already has patches). An empty box is
/// inside nothing.
pub fn half_inside(bbox: Rect, other: Rect) -> bool {
    let area = u64::from(bbox.w) * u64::from(bbox.h);
    if area == 0 {
        return false;
    }
    let w = (bbox.right().min(other.right()) - bbox.x.max(other.x)).max(0) as u64;
    let h = (bbox.bottom().min(other.bottom()) - bbox.y.max(other.y)).max(0) as u64;
    w * h * 2 >= area
}

/// `patch` with its coverage and its lettering cut to a detection's display
/// set, `mask ∪ ink`, and its mask digest recomputed; `None` when nothing lay
/// outside it. The bounds and the pixels stay as they were: a pixel with no
/// coverage is never composited ([`Job::complete_detection`]).
fn clipped_to_display(patch: &Patch, mask: &crate::mask::Mask, ink: &crate::mask::Mask) -> Option<Patch> {
    let shows = |x: i64, y: i64| mask.contains(x, y) || ink.contains(x, y);
    let clip = |source: &crate::mask::Mask| {
        let mut out = source.clone();
        let mut cut = false;
        for y in source.bounds.y..source.bounds.bottom() {
            for x in source.bounds.x..source.bounds.right() {
                if source.coverage(x, y) != 0 && !shows(x, y) {
                    out.set_coverage(x, y, 0);
                    cut = true;
                }
            }
        }
        (out, cut)
    };
    let (clipped_mask, mask_cut) = clip(&patch.mask);
    let (clipped_ink, ink_cut) = clip(&patch.ink);
    if !mask_cut && !ink_cut {
        return None;
    }
    let mut out = patch.clone();
    out.provenance.mask_sha256 = sha256_hex(&buffers::encode_mask(&clipped_mask));
    out.mask = clipped_mask;
    out.ink = clipped_ink;
    Some(out)
}

fn revision_from_buffer_ref(reference: &str) -> Option<String> {
    reference
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".buf"))
        .filter(|stem| stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
}

/// Manifest metadata for one prepared text-shaped mask revision. Pixel data
/// lives in bounded versioned sidecar artifacts; it is never embedded in the
/// JSON manifest. Rows are append-only by `(region_id, identity)` so undo and
/// redo can keep referring to the exact plan that produced a patch.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextShapePlanRecord {
    /// Version-five plans use the source's displayed orientation. Older masks
    /// stay in their original coordinates for exact rendering and undo.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub display_coordinates: bool,
    pub region_id: String,
    pub source_idx: usize,
    pub geometry_policy: GeometryPolicy,
    pub plan_version: u32,
    pub source_sha256: String,
    pub lower_composite_sha256: String,
    pub algorithm_id: String,
    pub model_id: Option<String>,
    pub candidate_bounds: Rect,
    pub refinement_crop: Rect,
    pub base_revision: u64,
    pub correction_revision: u64,
    pub plan_revision: u64,
    pub padding_px: u32,
    pub model_hole_margin_px: u32,
    pub reading_context: Rect,
    pub quality: MaskQualityState,
    pub identity: PlanIdentity,
    /// References are relative to the job sidecar directory.
    pub base_mask_ref: String,
    pub additions_ref: String,
    pub removals_ref: String,
    pub write_support_ref: String,
    pub model_hole_ref: String,
    pub blend_alpha_ref: Option<String>,
}

/// Latest failed text-shaped preparation for a region. Failed previews have
/// no approved W and therefore no prepared sidecar, but their correction
/// state must survive save/reopen until a newer plan is accepted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextShapeCorrectionRecord {
    pub region_id: String,
    pub source_idx: usize,
    pub plan_version: u32,
    pub plan_revision: u64,
    pub candidate_bounds: Rect,
    pub reason: String,
}

impl TextShapePlanRecord {
    fn same_identity(&self, region_id: &str, identity_sha256: &str) -> bool {
        self.region_id == region_id && self.identity.identity_sha256 == identity_sha256
    }
}

/// §1's `regions_untouched`: declined and gate-dropped regions.
///
/// They are persisted because they are the review list's other half. A region
/// the gate held back has no patch, so without this row a resumed job shows a
/// clean page and no record that anything was deliberately left alone.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegionUntouched {
    pub source_idx: usize,
    pub bbox: Rect,
    /// An i18n key - `review.reason.*` or `decline.reason.*`. No English
    /// crosses the seam, and none is written to disk either.
    pub reason: String,
    /// Set on a held text-group candidate only: whether grouping found its
    /// lettering inside a balloon. It decides which engine a clean the user
    /// asks for starts on (the in-bubble pick inside one, the outside pick
    /// otherwise), natively and in the interface alike. Absent on every other
    /// row and on rows written before it existed, which start outside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside_bubble: Option<bool>,
    /// Set on a held text-group candidate and on a region the quality metric
    /// declined: the lettering grouping held, or the fit started from, so a
    /// clean the user asks for writes through those glyphs rather than the box
    /// around them, which on artwork no rung can fill. Absent on every other
    /// row, on a candidate with no lettering (a weak text box) and on rows
    /// written before it existed, which look under the box as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lettering: Option<HeldLettering>,
}

/// A held candidate's lettering on disk.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HeldLettering {
    /// `candidates/{digest}.mask` within `<job>.mtclean.d/`, named by the
    /// encoded mask's digest, so two rows over one set of glyphs share a file
    /// and a file never changes under the row that names it. Page pixels.
    pub mask_ref: String,
    /// The proxy scale of the segmentation that found it, which the fit grows
    /// it by, as a run's fit grows a cleaned group's lettering.
    pub scale: f32,
}

/// An untouched row on its way to the manifest, with the lettering a held
/// candidate keeps (`None` for every other row). The writer stores the mask
/// and names it in [`RegionUntouched::lettering`] in the same write.
#[derive(Debug, Clone)]
pub struct PendingUntouched {
    pub row: RegionUntouched,
    pub lettering: Option<(crate::mask::Mask, f32)>,
}

impl From<RegionUntouched> for PendingUntouched {
    fn from(row: RegionUntouched) -> PendingUntouched {
        PendingUntouched { row, lettering: None }
    }
}

/// A region Detect found and fitted, stored so that a later Clean cleans it
/// without detecting again (`docs/detect-clean.md` §2).
///
/// **A real region, with nothing cleaned yet.** It survives a reopen and it is
/// what every later clean starts from, local or cloud, so the page is not read
/// by a detector twice. It has no pixels of its own: the only sidecar files it
/// owns are its fitted mask and the lettering under it, which is why deleting
/// one removes it outright rather than turning a flag off.
///
/// Only regions the gate would clean become one of these. A region the gate or
/// the language choice held back stays a [`RegionUntouched`] row, as a
/// one-pass run leaves it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DetectedRegion {
    /// `{page id}-d{n}`, `n` from [`Counters::detections`]. The patch or
    /// untouched row a clean replaces it with keeps this id.
    pub id: String,
    pub source_idx: usize,
    /// The detected text box, in page pixels.
    pub bbox: Rect,
    /// `detections/{id}.mask` within `<job>.mtclean.d/`: the mask the fit
    /// settled on, which is the one the cleaner writes through. The lettering
    /// ([`crate::fit::Fitted::ink`]) sits beside it as `.ink`
    /// ([`DetectedRegion::ink_ref`]). Their own subdirectory, because a
    /// cleaned patch writes `<id>.ink` at the sidecar root and the two must
    /// never name one file.
    pub mask_ref: String,
    /// Inside a detected balloon: the question the two picks are told apart by.
    pub inside: bool,
    /// The balloon fill the run measured: the paper ring's median tone, for a
    /// region inside a balloon. `None` outside one.
    #[serde(default)]
    pub balloon_color: Option<[u8; 3]>,
    /// The script the gate read: `ja`, `zh` or `ko`. `None` for text the run
    /// cleans without reading it (the all-text policy, or opted-in outside
    /// text).
    #[serde(default)]
    pub script: Option<String>,
    /// The rung the run would start this region at: `fill`, `solid` or
    /// `lama`. A stored `denoise` reads as `fill` ([`read_pick`]).
    #[serde(deserialize_with = "read_pick")]
    pub pick: String,
    /// `local`, or `cloud` when any cloud stage contributed to this page.
    pub detector: String,
    /// Seconds since the epoch.
    pub created: u64,
    /// The digest of the encoded mask file, the same digest a patch records
    /// for the mask it was applied with.
    #[serde(default)]
    pub mask_sha256: String,
    /// The region's place in reading order on its page, which is the order a
    /// one-pass run would have given its patch.
    #[serde(default)]
    pub order: u32,
    /// The `review.reason.*` key the patch is to be flagged under once cleaned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_state: Option<String>,
    /// What the fit decided beyond the mask. Absent for a detection whose fit
    /// was not recorded; a clean then treats the stored mask as
    /// authoritative, as a hand mask is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<DetectionFit>,
    /// The text group this detection is: stable group identity, detector
    /// provenance, explicit review reasons and a reference to the page's raw
    /// evidence file ([`crate::text_groups`]). Absent on detections made
    /// before grouping and on the legacy box path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<crate::text_groups::GroupRecord>,
    /// Total growth in page pixels from segmentation when `padding_from_seed`
    /// is true. Legacy records instead measured extra growth from fitted masks.
    /// Above zero the ungrown pair is stored beside the output, so changing
    /// padding never compounds dilation and zero restores the baseline exactly.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub padding_px: u32,
    /// True when padding is measured from segmentation instead of the legacy fitted masks.
    #[serde(default)]
    pub padding_from_seed: bool,
}

/// The fit's own answers a clean from a stored mask cannot re-derive: the route
/// and the growth came out of the candidate search, which a clean does not run,
/// and `scale` is what places the engine window exactly where the detecting
/// run placed it, so the ring is measured over the same paper.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DetectionFit {
    pub route: crate::fit::Route,
    pub thickness: u32,
    pub best_deviation: f32,
    /// The detection segmentation's proxy scale.
    pub scale: f32,
}

impl DetectedRegion {
    /// The sidecar file holding the lettering: the mask's own stem with `.ink`.
    pub fn ink_ref(&self) -> String {
        self.mask_ref
            .strip_suffix(".mask")
            .map(|stem| format!("{stem}.ink"))
            .unwrap_or_else(|| format!("detections/{}.ink", self.id))
    }

    /// The sidecar file holding the mask before [`DetectedRegion::padding_px`]
    /// grew it: the mask's own stem with `.base.mask`, and the mask itself
    /// while the padding is 0.
    pub fn base_mask_ref(&self) -> String {
        self.base_ref("mask").unwrap_or_else(|| self.mask_ref.clone())
    }

    /// The lettering before the padding grew it, as
    /// [`DetectedRegion::base_mask_ref`] names the mask.
    pub fn base_ink_ref(&self) -> String {
        self.base_ref("ink").unwrap_or_else(|| self.ink_ref())
    }

    fn base_ref(&self, ext: &str) -> Option<String> {
        (self.padding_px > 0).then(|| {
            self.mask_ref
                .strip_suffix(".mask")
                .map(|stem| format!("{stem}.base.{ext}"))
                .unwrap_or_else(|| format!("detections/{}.base.{ext}", self.id))
        })
    }

    /// Every sidecar file the record names: its mask and lettering, and their
    /// ungrown pair when it is padded.
    fn files(&self) -> Vec<String> {
        let mut files = vec![self.mask_ref.clone(), self.ink_ref()];
        if self.padding_px > 0 {
            files.extend([self.base_mask_ref(), self.base_ink_ref()]);
        }
        files
    }

    /// How many hand edits its masks have had: the `n` of the
    /// `detections/{id}.e{n}.mask` [`Job::rewrite_page_detections`] names, and
    /// 0 for the files Detect wrote. Each edit writes new files under a new
    /// number, so the number and the id name one mask and one lettering.
    pub fn edits(&self) -> u64 {
        self.mask_ref
            .strip_prefix(&format!("detections/{}.e", self.id))
            .and_then(|rest| rest.strip_suffix(".mask"))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }
}

/// A detection's stored pick, with rung 1's name read as rung 0's. Denoise fill
/// was removed once pages were denoised whole; a project detected before that
/// still names it, and its regions now start on the fill.
fn read_pick<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let pick = <String as serde::Deserialize>::deserialize(deserializer)?;
    Ok(if pick == "denoise" { "fill".into() } else { pick })
}

/// One file page denoise wrote for one page, and the page it was made from.
/// The newest row of a page ([`Project::current_denoised`]) is what lets the
/// chapter take that file as its page ([`Job::replace_source`]), and only
/// while the file is still a picture of the page as it is now. The older rows
/// are the chapter's denoise history: what was run, how, and whether it was
/// taken, which the chapter list shows and compares against the raw page.
///
/// Rows that share `created` are one run. Every field after `created` is
/// defaulted, so a row written before the history was kept still opens, as a
/// run with no preset or target to name.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DenoisedPage {
    pub source_idx: usize,
    /// The file denoise wrote. Absolute: it is in a folder the user chose,
    /// outside the library.
    pub path: PathBuf,
    /// The page's appearance when denoise read it: the desktop's cache
    /// identity of the source and every visible patch drawn on it. Opaque
    /// here; compared, never parsed.
    pub appearance: String,
    /// Seconds since the epoch. Also the run's name: [`Job::record_denoised`]
    /// keeps two runs from sharing one.
    pub created: u64,
    /// The shipped preset the run used, or `None` for a cloud recipe that is
    /// no preset and for a row written before presets were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Where the run ran, `None` on an old row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<crate::cloud_denoise_wire::PresetTarget>,
    /// Whether visible patches reached the page when it was read, so the file
    /// has the cleaning in it. `None` on an old row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_cleaned: Option<bool>,
    /// The page's source file when denoise read it, relative to the job's
    /// root as [`SourceRef::rel_path`] is. The raw page this file compares
    /// against, which is no longer the page's source once the file was taken.
    /// `None` on an old row, whose raw page is the source it still has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    /// Set by [`Job::replace_source`] when this file was taken as the page.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub taken: bool,
}

/// How many denoise runs a manifest remembers. A run is the rows sharing one
/// `created`; past this many, the oldest run's rows go, so a chapter denoised
/// every week does not grow its manifest for ever.
pub const MAX_DENOISE_RUNS: usize = 10;

/// The index in `rows` of the newest row for `source_idx`: the largest
/// `created`, and the later row of two with the same one.
fn newest_denoised(rows: &[DenoisedPage], source_idx: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.source_idx == source_idx)
        .max_by_key(|(_, row)| row.created)
        .map(|(at, _)| at)
}

/// A stored detection with its two masks read back, in page pixels.
#[derive(Debug, Clone)]
pub struct LoadedDetection {
    pub record: DetectedRegion,
    pub mask: crate::mask::Mask,
    pub ink: crate::mask::Mask,
    /// The page evidence file the record's group refers to, when this
    /// detection was just made and the file is not yet written. Never read
    /// back: [`Job::load_detection`] leaves it `None`.
    pub evidence: Option<crate::text_groups::EvidenceBlob>,
    /// The mask and lettering before the record's
    /// [`DetectedRegion::padding_px`] grew them, written beside them. A
    /// write of a padded record needs it; [`Job::load_detection`] leaves it
    /// `None`, and [`Job::load_detection_base`] reads it.
    pub base: Option<(crate::mask::Mask, crate::mask::Mask)>,
}

/// One file that ingest refused, and why.
///
/// The file name rather than the path: a skipped file has no source row to
/// hang a `rel_path` on, and what the report says is "`wm014.jpg` was skipped".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkippedInput {
    pub file: String,
    /// An `input.skipReason.*` key - [`crate::ingest::SkipReason::reason_key`].
    pub reason: String,
}

/// §2's input report, kept for the same reason §1 keeps `regions_untouched`.
///
/// §2 requires every refused file to be "skipped and **listed**", and the seam
/// carries that list as `ApiChapter.inputReports`. Neither survives the session
/// that produced it unless it is written down: the accepted files become
/// `sources` and the refused ones become nothing at all, so reopening a chapter
/// would show twenty-two pages where twenty-four files sit and no record that
/// two were refused. Re-deriving it means re-hashing and re-decoding every file
/// in the folder on every open, which is the cost §1 persists `counters` to
/// avoid.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InputReport {
    pub skipped: Vec<SkippedInput>,
    /// Dotfiles, `__MACOSX/`, `Thumbs.db`, `desktop.ini`. A count rather than a
    /// list, because the notice that reports it is a count.
    pub junk_skipped: u32,
    /// §2's "warn rather than guess": the stems that appeared under two
    /// extensions.
    pub duplicate_basenames: Vec<String>,
    /// The files ingest converted on the way in. Persisted for the same reason
    /// as `skipped`: it is a thing that happened to the user's folder, and
    /// re-deriving it on open would mean re-reading every file to find out
    /// what format it used to be.
    pub converted: Vec<ConvertedInput>,
}

/// One row of [`InputReport::converted`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ConvertedInput {
    /// The original file's name, which is the one the user recognises.
    pub file: String,
    /// `JPEG`, `WebP`, `GIF`, `BMP` - not a translated string, interpolated
    /// into one that is.
    pub from: String,
}

impl InputReport {
    /// The report an ingest produced, in the form the manifest keeps.
    pub fn of(report: &IngestReport) -> InputReport {
        InputReport {
            skipped: report
                .skipped
                .iter()
                .filter(|entry| entry.reason != crate::ingest::SkipReason::Junk)
                .map(|entry| SkippedInput {
                    file: file_name(&entry.path),
                    reason: entry.reason.reason_key().to_owned(),
                })
                .collect(),
            junk_skipped: report.junk_skipped() as u32,
            duplicate_basenames: report
                .warnings
                .iter()
                .map(|warning| match warning {
                    Warning::DuplicateBasename { stem, .. } => stem.clone(),
                })
                .collect(),
            converted: report
                .converted
                .iter()
                .map(|entry| ConvertedInput {
                    file: file_name(&entry.original),
                    from: entry.from.to_owned(),
                })
                .collect(),
        }
    }
}

/// §1's `counters`, verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Counters {
    pub refused: u32,
    pub errored: u32,
    pub param_reject: u32,
    pub residual_reject: u32,
    pub structural_reject: u32,
    pub declined: u32,
    pub gate_dropped: u32,
    /// The next detection's `n` in `{page id}-d{n}`. **An allocator, not a
    /// tally**: it only goes up, so an id a cleaned detection handed to its
    /// patch is never minted again. Left off the file while it is zero, so a
    /// manifest nothing has detected on is written byte for byte as before.
    #[serde(skip_serializing_if = "is_zero")]
    pub detections: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// The whole `.mtclean` file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Project {
    /// Preserve fields written by other compatible builds when saving edits.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    pub version: u32,
    /// Seconds since the Unix epoch.
    pub created: u64,
    pub app_version: String,
    pub sources: Vec<ProjectSource>,
    pub strip: Strip,
    pub patches: Vec<PatchRecord>,
    /// Prepared text-shaped geometry, including plans that have a preview but
    /// have not produced an applied patch yet. Absent from old projects, where
    /// the policy remains legacy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_shape_plans: Vec<TextShapePlanRecord>,
    /// Earlier committed patch records retain their own immutable sidecars so
    /// a correction or rerun can be undone after a later revision is saved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_shape_patch_revisions: Vec<PatchRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legacy_patch_revisions: Vec<PatchRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_shape_corrections: Vec<TextShapeCorrectionRecord>,
    pub regions_untouched: Vec<RegionUntouched>,
    /// Regions Detect stored and nothing has cleaned yet. Absent from a
    /// manifest no Detect has written to, and skipped while empty, so old
    /// projects open and flush unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detections: Vec<DetectedRegion>,
    /// The files page denoise wrote, one row per page per run, for the
    /// newest [`MAX_DENOISE_RUNS`] runs ([`Job::record_denoised`]). Absent
    /// from a manifest no denoise has written to, and skipped while empty, so
    /// old projects open and flush unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denoised: Vec<DenoisedPage>,
    pub counters: Counters,
    /// What §2 refused on the way in, and what it warned about.
    ///
    /// Not in §1's field list; added because §2's listing and the seam's
    /// `ApiChapter.inputReports` both outlive the ingest that produced them.
    /// Defaulted on read, so a manifest written before it existed still opens.
    #[serde(default)]
    pub input_report: InputReport,
    /// Which sources the detector has actually looked at, as indices into
    /// `sources`.
    ///
    /// Not in §1's field list, and the one bit no other field can carry. Every
    /// other record here describes work that *produced* something: a patch, an
    /// untouched region, a counter above zero. A page the detector examined and
    /// found no text on produces none of them, so without this field it is
    /// indistinguishable from a page nothing has ever looked at - and the two
    /// want opposite behaviour. The first is finished and must not be queued
    /// again; the second is the entire remaining job.
    ///
    /// It is also the only thing that can answer the seam's
    /// `ApiChapter.noTextDetected`, which is a claim about what was *found*
    /// and therefore presupposes that something looked. Defaulted on read, so a
    /// manifest written before it existed still opens - as an unexamined
    /// chapter, which is the safe reading of a job whose run predates the
    /// record.
    #[serde(default)]
    pub examined: Vec<usize>,
    /// Which sources a run failed on, as indices into `sources`.
    ///
    /// The same shape as `examined` and there for the same kind of reason:
    /// `counters.errored` is a number, and a number cannot be re-derived. A
    /// page that could not be read or could not be cleaned produces no patch
    /// and no untouched row, is deliberately **not** marked examined so a later
    /// run tries it again, and so was counted afresh on every attempt - the
    /// counter climbed for ever over one corrupt file. This is the record the
    /// counter is kept in step with: a page enters the list when it fails and
    /// leaves it when it is re-attempted, and `counters.errored` is the length
    /// of that state rather than a tally of attempts.
    ///
    /// Cleared for a source whose hash changed, alongside `examined`, and
    /// defaulted on read like the three fields above it.
    #[serde(default)]
    pub errored: Vec<usize>,
    /// Which sources a Detect run has finished, as indices into `sources`.
    ///
    /// Detect's counterpart of `examined`. Detect looks at a page and stores
    /// what it found for a later clean, so the page is not examined yet; and a
    /// page it found no text on stores nothing at all. Without this record a
    /// Detect that stopped part way - cancelled, or ended by a cloud failure -
    /// could not tell its finished pages from the rest, and the next Detect
    /// started the chapter over, sending every page to the cloud again.
    ///
    /// Cleared for a source whose hash changed, alongside `examined`.
    /// Defaulted on read and skipped while empty, so old projects open and
    /// flush unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detected: Vec<usize>,
    /// Where a cancelled or crashed run stopped, as a position in
    /// `strip.order`, or `None` for a job that is not interrupted.
    ///
    /// §6 says "on launch, any incomplete job offers a resume" and §1 gives the
    /// manifest no way to say that a job **is** incomplete: every field it
    /// holds describes work that finished. This is the one bit that does not,
    /// and it is what `resumeJob` and `ApiProject.interruptedJob` are read
    /// from. Defaulted on read for the same reason as `input_report`.
    #[serde(default)]
    pub interrupted_at: Option<u32>,
    /// A snapshot of the settings the job ran under.
    ///
    /// Opaque here on purpose: the settings vocabulary belongs to the interface
    /// (`readSettings`/`writeSettings`), and duplicating it in the core
    /// would give two definitions to disagree with each other. What the core
    /// owes is that the snapshot survives a round trip unchanged.
    pub settings: serde_json::Value,
}

impl Project {
    fn schema_version(&self) -> u32 {
        if self.sources.iter().any(|source| source.orientation.0 != 1)
            || self.patches.iter().chain(self.legacy_patch_revisions.iter()).chain(self.text_shape_patch_revisions.iter()).any(|record| !record.native_parts.is_empty()) { 5 }
        else if self.requires_v4() { 4 } else { 3 }
    }
    fn requires_v4(&self) -> bool {
        !self.text_shape_plans.is_empty()
            || !self.text_shape_corrections.is_empty()
            || self.patches.iter().any(|row| row.geometry_policy == GeometryPolicy::TextShape)
            || !self.text_shape_patch_revisions.is_empty()
    }

    /// A new project over a set of ingested sources.
    ///
    /// Takes [`SourceRef`]s because that is what [`crate::ingest`] produces and
    /// what §2's policy has already filtered - a project is never built from
    /// paths that have not been through it.
    pub fn new(
        manifest_dir: &Path,
        app_version: &str,
        mode: StripMode,
        sources: &[SourceRef],
    ) -> Project {
        let rows: Vec<ProjectSource> = sources
            .iter()
            .map(|source| ProjectSource {
                extra: Default::default(),
                orientation: source.orientation,
                rel_path: relative_to(manifest_dir, &source.path),
                sha256: source.sha256.clone(),
                mtime: mtime_of(&source.path),
                w: source.width,
                h: source.height,
                mode: source.mode,
                bit_depth: source.bit_depth,
                conversion: source.conversion.clone().map(|mut provenance| {
                    provenance.archived_original = relative_to(manifest_dir, &provenance.archived_original);
                    provenance
                }),
                converted_from: source
                    .converted_from
                    .as_ref()
                    .map(|original| relative_to(manifest_dir, original)),
            })
            .collect();

        Project {
            extra: Default::default(),
            version: 3,
            created: now(),
            app_version: app_version.to_owned(),
            strip: Strip {
                mode,
                order: (0..rows.len()).collect(),
                splits: Vec::new(),
            },
            sources: rows,
            patches: Vec::new(),
            text_shape_plans: Vec::new(),
            text_shape_patch_revisions: Vec::new(),
            legacy_patch_revisions: Vec::new(),
            text_shape_corrections: Vec::new(),
            regions_untouched: Vec::new(),
            detections: Vec::new(),
            denoised: Vec::new(),
            counters: Counters::default(),
            input_report: InputReport::default(),
            examined: Vec::new(),
            errored: Vec::new(),
            detected: Vec::new(),
            interrupted_at: None,
            settings: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// A new project over a whole ingest, refusals included.
    ///
    /// The constructor to reach for when the caller has an
    /// [`IngestReport`] rather than a bare source list: it is the only way the
    /// refused files reach the manifest, and a project built with [`new`] from
    /// `report.sources` alone drops them silently.
    ///
    /// [`new`]: Project::new
    pub fn from_ingest(
        manifest_dir: &Path,
        app_version: &str,
        mode: StripMode,
        report: &IngestReport,
    ) -> Project {
        let mut project = Project::new(manifest_dir, app_version, mode, &report.sources);
        project.input_report = InputReport::of(report);
        project
    }

    /// Which source has these bytes, if any. The byte-level half of "output
    /// never overwrites input": a destination path can be anything, but a file
    /// whose content is one of ours is one of ours.
    pub fn source_with_hash(&self, sha256: &str) -> Option<usize> {
        self.sources
            .iter()
            .position(|source| source.sha256 == sha256)
    }

    /// Whether anything cleaned this source or began to: a patch, shown or
    /// hidden, a prepared text-shaped plan, a kept revision or a correction.
    /// Each was made against the source's present bytes and checks them by
    /// digest, so a page with any of them keeps its source
    /// ([`Job::replace_source`]).
    pub fn has_cleaning(&self, source_idx: usize) -> bool {
        self.patches.iter().any(|row| row.source_idx == source_idx)
            || self.text_shape_plans.iter().any(|row| row.source_idx == source_idx)
            || self.text_shape_patch_revisions.iter().any(|row| row.source_idx == source_idx)
            || self.legacy_patch_revisions.iter().any(|row| row.source_idx == source_idx)
            || self.text_shape_corrections.iter().any(|row| row.source_idx == source_idx)
    }

    /// The page's current denoised file: its newest row, unless that row was
    /// already taken as the page. An older row is history, never a candidate:
    /// it was made before the newest run and, after a take, from a source the
    /// page no longer has. Everything that asks "the page's denoised file"
    /// asks here.
    pub fn current_denoised(&self, source_idx: usize) -> Option<&DenoisedPage> {
        let row = &self.denoised[newest_denoised(&self.denoised, source_idx)?];
        (!row.taken).then_some(row)
    }
}

/// Why [`Job::replace_source`] kept a page's source. Nothing was written.
#[derive(Debug, thiserror::Error)]
pub enum ReplaceError {
    #[error("no source {0}")]
    NoSource(usize),
    #[error("the page has cleaning on it")]
    Cleaned,
    #[error("not a readable PNG: {0}")]
    Unreadable(String),
    #[error("the file is {found_w}x{found_h}; the page is {w}x{h}")]
    SizeChanged { w: u32, h: u32, found_w: u32, found_h: u32 },
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a write was refused. §1: "Refuse to write to any path whose sha256 is in
/// `sources`; default the output directory to a sibling `<input>_cleaned/`."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputRefusal {
    /// The destination directory is where sources live. Refused as a directory
    /// rather than per file, because an export writes a set and "some of them
    /// would have been overwritten" is not a state to leave a chapter in.
    SourceDirectory { source_idx: usize },
    /// A specific destination file resolves to a source.
    WouldOverwriteSource { source_idx: usize },
}

impl OutputRefusal {
    /// The key the seam reports this under.
    /// `exportChapter` returns
    /// `status: 'refused'` with a `reasonKey`, and the interface already has
    /// this one.
    pub fn reason_key(&self) -> &'static str {
        "notice.export.refusedOverwrite"
    }
}

/// What re-verification found for one source. §6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceState {
    Unchanged,
    Missing,
    /// The file is there and is not what it was.
    Changed {
        sha256: String,
    },
}

impl SourceState {
    pub fn is_stale(&self) -> bool {
        !matches!(self, SourceState::Unchanged)
    }
}

/// What a resume dropped, so the user can be told rather than left to notice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StaleReport {
    pub sources: Vec<usize>,
    pub dropped_patches: Vec<String>,
}

impl StaleReport {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// An open job: the manifest, its sidecar directory, and the flush.
pub struct Job {
    path: PathBuf,
    dir: PathBuf,
    pub project: Project,
    /// The generation this job stands on: the sha256 of the manifest bytes it
    /// last read or wrote, or `None` for a manifest that did not exist. A save
    /// goes ahead only while the file on disk is still that generation.
    ///
    /// A digest rather than a counter in the file, for two reasons. It needs no
    /// change to the manifest's bytes, which old projects and older builds
    /// depend on; and it catches every writer, including an older build or a
    /// tool that knows nothing of any counter. Two writes that happen to leave
    /// identical bytes are one generation, and replacing one with the other
    /// loses nothing.
    seen: Mutex<Option<String>>,
    /// Set once a save was refused as stale. Every later save from this job
    /// would be refused too; the run reads this to open the job again.
    stale: AtomicBool,
    /// Opened by [`Job::open_read_only`]: every write is refused before it
    /// takes the lock or touches a file.
    read_only: bool,
}

/// The generation on disk: the digest of the manifest's bytes, `None` when
/// there is no manifest.
fn generation_of(path: &Path) -> Result<Option<String>, StoreError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(sha256_hex(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StoreError::io(path, error)),
    }
}

impl Job {
    /// Create a job and write its first manifest.
    ///
    /// The manifest exists on disk before any region runs, so a crash during
    /// the first region still leaves something to resume from - a job that only
    /// appears once it has produced output is a job that cannot report having
    /// produced none.
    ///
    /// Creating is starting over by definition, so whatever manifest is at
    /// `path` is replaced; the replacement is made under the job's lock.
    pub fn create(path: &Path, project: Project) -> Result<Job, StoreError> {
        let job = Job {
            path: path.to_path_buf(),
            dir: sidecar_dir(path),
            project,
            seen: Mutex::new(None),
            stale: AtomicBool::new(false),
            read_only: false,
        };
        std::fs::create_dir_all(&job.dir).map_err(|e| StoreError::io(&job.dir, e))?;
        let _write = lock::hold(&job.path)?;
        *job.seen() = generation_of(&job.path)?;
        job.flush()?;
        Ok(job)
    }

    /// Open a job, refusing a manifest this build cannot read.
    ///
    /// The version is read **first**, from a probe that knows about one field,
    /// and the rest is deserialised only once the number matches. Reading it
    /// off a fully decoded [`Project`] instead would mean a manifest from a
    /// later version could only be recognised as one where its *shape* had not
    /// changed - every version that moved a field would arrive as
    /// [`StoreError::Json`], which says "not readable as JSON" about a file
    /// that is perfectly good JSON from a build that is not this one.
    pub fn open(path: &Path) -> Result<Job, StoreError> {
        let _detection_io = detection_io().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // The orphan sweep below deletes detection files the manifest does not
        // name, which is only safe while nobody can be between writing such a
        // file and saving the manifest that names it. That is true when this
        // thread holds the job, or when the job can be taken right now; it is
        // taken *before* the read, so the manifest swept against is current.
        // Another thread or process holding it is mid-write, perhaps, and the
        // sweep waits for a later open. A job with no manifest is not locked
        // at all: the read below refuses it, and a lock would leave a
        // `.lock` beside every chapter a caller only looked for.
        let sweep = if lock::held_by_current_thread(path) {
            Some(None)
        } else if path.is_file() {
            lock::try_lock(path).map(Some)
        } else {
            None
        };
        let bytes = std::fs::read(path).map_err(|e| StoreError::io(path, e))?;
        let job = Job::parse(path, &bytes, false)?;
        // Maintenance cannot make an otherwise readable manifest refuse to
        // open, for example when its sidecar is on a read-only volume.
        if sweep.is_some() {
            let _ = job.sweep_detection_files();
            let _ = job.sweep_evidence_files();
            let _ = job.sweep_held_lettering_files();
        }
        Ok(job)
    }

    /// Open a job only to read it: the manifest is read and checked exactly
    /// as [`Job::open`] reads it, but no lock is taken (so no `.lock` file is
    /// made beside it), no orphaned detection file is swept, and every write
    /// the job is then asked for is refused ([`StoreError::ReadOnly`]). For
    /// tools that inspect a chapter and must leave it byte for byte as found.
    pub fn open_read_only(path: &Path) -> Result<Job, StoreError> {
        let bytes = std::fs::read(path).map_err(|e| StoreError::io(path, e))?;
        Job::parse(path, &bytes, true)
    }

    /// The job a manifest's `bytes` describe, version checked first.
    fn parse(path: &Path, bytes: &[u8], read_only: bool) -> Result<Job, StoreError> {
        #[derive(serde::Deserialize)]
        struct VersionOnly {
            version: u32,
        }
        let probe: VersionOnly =
            serde_json::from_slice(bytes).map_err(|e| StoreError::Json(e.to_string()))?;
        if !(OLDEST_READABLE_VERSION..=FORMAT_VERSION).contains(&probe.version) {
            return Err(StoreError::Version {
                expected: FORMAT_VERSION,
                found: probe.version,
            });
        }

        let mut project: Project =
            serde_json::from_slice(bytes).map_err(|e| StoreError::Json(e.to_string()))?;
        let document: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| StoreError::Json(e.to_string()))?;
        let root = path.parent().unwrap_or(Path::new("."));
        for (index, source) in project.sources.iter_mut().enumerate() {
            if document.get("sources").and_then(|v| v.get(index)).and_then(|v| v.get("orientation")).is_none() {
                if let Ok(source_bytes) = std::fs::read(root.join(&source.rel_path)) {
                    source.orientation = crate::image::orientation::from_bytes(&source_bytes);
                }
            }
        }
        if project.sources.iter().any(|s| s.orientation.0 != 1) {
            for record in project.patches.iter_mut().chain(project.legacy_patch_revisions.iter_mut()).chain(project.text_shape_patch_revisions.iter_mut()) {
                if record.native_parts.is_empty() && record.native_order.is_empty() { record.native_order = project.strip.order.clone(); }
            }
        }
        // The v3 reader ignores added legacy metadata. Only text-shaped rows
        // and plans require v4; older versions still advance to the v3 floor.
        project.version = project.schema_version();
        Ok(Job {
            path: path.to_path_buf(),
            dir: sidecar_dir(path),
            project,
            seen: Mutex::new(Some(sha256_hex(bytes))),
            stale: AtomicBool::new(false),
            read_only,
        })
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The generation this job stands on: the digest of the manifest it last
    /// read or wrote. For diagnostics; [`Job::flush`] is what enforces it.
    pub fn generation(&self) -> Option<String> {
        self.seen().clone()
    }

    /// Whether a save from this job was refused because the manifest had
    /// moved on. Such a job cannot save again; open the manifest afresh.
    pub fn went_stale(&self) -> bool {
        self.stale.load(Ordering::SeqCst)
    }

    /// Refuse, before anything is written, when the manifest on disk is not
    /// the generation this job stands on. Called under the job's lock, so the
    /// answer holds until the write that follows it.
    fn check_fresh(&self) -> Result<(), StoreError> {
        if generation_of(&self.path)? != *self.seen() {
            self.stale.store(true, Ordering::SeqCst);
            return Err(StoreError::Stale { path: self.path.clone() });
        }
        Ok(())
    }

    /// The start of every write: the job's lock for the rest of it, unless
    /// this thread holds it already, and the stale check before any sidecar
    /// is touched. A sidecar written from a stale job could replace a file
    /// the newer manifest names.
    fn begin_write(&self) -> Result<Option<lock::JobLock>, StoreError> {
        if self.read_only {
            return Err(StoreError::ReadOnly { path: self.path.clone() });
        }
        let held = lock::hold(&self.path)?;
        self.check_fresh()?;
        Ok(held)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where patch buffers and masks live: a sibling `<job>.mtclean.d/`.
    pub fn sidecar(&self) -> &Path {
        &self.dir
    }

    /// The directory `rel_path` is resolved against.
    pub fn root(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    pub fn source_path(&self, index: usize) -> Option<PathBuf> {
        self.project
            .sources
            .get(index)
            .map(|source| self.root().join(&source.rel_path))
    }

    /// Where the page **came from**, which since ingest began importing pages
    /// is never where its file is.
    ///
    /// [`source_path`] names a file inside the job's own sidecar, and every
    /// question about the *user's* folder - where the export's
    /// `<input>_cleaned/` sibling goes, which directory the chapter says it was
    /// made from, which files an export must not land on - wants the scan
    /// instead. The path is returned whether or not it still exists, because
    /// every one of those questions is about a *directory the user chose* and
    /// none of them opens the file.
    ///
    /// A manifest from a build before the import falls back to `rel_path`,
    /// where the two genuinely were the same file.
    ///
    /// [`source_path`]: Job::source_path
    pub fn origin_path(&self, index: usize) -> Option<PathBuf> {
        let source = self.project.sources.get(index)?;
        Some(
            self.root()
                .join(source.converted_from.as_ref().unwrap_or(&source.rel_path)),
        )
    }

    /// Rewrite the manifest atomically, under the job's lock, and only over
    /// the generation this job read or last wrote.
    ///
    /// A caller that holds the job ([`lock::lock`], which is what
    /// `run::lock_job` takes) never meets [`StoreError::Stale`]: nobody else
    /// can have saved in between. One that does not hold it has the lock taken
    /// for it here, and a manifest that moved since it was read is refused
    /// rather than overwritten. That is the lost update this exists to stop: a
    /// writer that read generation N saving over N+k would roll back every
    /// commit in between.
    ///
    /// The manifest is looked at twice: before anything is written, and again
    /// once the new one is synced under its temporary name, right before the
    /// rename. The lock keeps every writer that takes it out of the time in
    /// between; the second look is for one that does not (an older build, a
    /// tool), and it leaves such a writer only the rename itself to slip into.
    pub fn flush(&self) -> Result<(), StoreError> {
        self.flush_staged(|| {})
    }

    /// [`Job::flush`], running `staged` once the new manifest is synced under
    /// its temporary name, which is where a test puts a writer racing it.
    fn flush_staged(&self, staged: impl FnOnce()) -> Result<(), StoreError> {
        let _write = self.begin_write()?;
        let mut project = self.project.clone();
        project.version = project.schema_version();
        let bytes = serde_json::to_vec_pretty(&project)
            .map_err(|e| StoreError::Json(e.to_string()))?;
        let pending = buffers::stage(&self.path, &bytes)?;
        staged();
        self.check_fresh()?;
        pending.publish()?;
        *self.seen() = Some(sha256_hex(&bytes));
        Ok(())
    }

    pub fn text_shape_correction(&self, region_id: &str) -> Option<&TextShapeCorrectionRecord> {
        self.project
            .text_shape_corrections
            .iter()
            .find(|record| record.region_id == region_id)
    }

    /// Save the latest review state without fabricating an approved support.
    /// The mask plan itself remains in the caller until a newer valid revision
    /// can be stored with `store_text_shape_plan`.
    pub fn record_text_shape_correction(
        &mut self,
        source_idx: usize,
        region_id: &str,
        plan_revision: u64,
        candidate_bounds: Rect,
        reason: &str,
    ) -> Result<(), StoreError> {
        if self.project.sources.get(source_idx).is_none()
            || region_id.is_empty()
            || reason.is_empty()
        {
            return Err(StoreError::Malformed(
                "invalid text-shape correction state".into(),
            ));
        }
        if self.text_shape_correction(region_id).is_some_and(|record| {
            record.source_idx != source_idx || plan_revision < record.plan_revision
        }) {
            return Err(StoreError::StalePlan);
        }
        self.project
            .text_shape_corrections
            .retain(|record| record.region_id != region_id);
        let source = &self.project.sources[source_idx];
        let safe_locator = candidate_bounds.w > 0
            && candidate_bounds.h > 0
            && candidate_bounds.x >= 0
            && candidate_bounds.y >= 0
            && candidate_bounds
                .x
                .checked_add(candidate_bounds.w as i64)
                .is_some_and(|right| right <= source.w as i64)
            && candidate_bounds
                .y
                .checked_add(candidate_bounds.h as i64)
                .is_some_and(|bottom| bottom <= source.h as i64)
            && u64::from(candidate_bounds.w) * u64::from(candidate_bounds.h)
                <= crate::text_shape::MAX_PLAN_PIXELS as u64;
        let candidate_bounds = if safe_locator {
            candidate_bounds
        } else {
            Rect::new(
                (source.w / 2) as i64,
                (source.h / 2) as i64,
                source.w.min(1),
                source.h.min(1),
            )
        };
        self.project
            .text_shape_corrections
            .push(TextShapeCorrectionRecord {
                region_id: region_id.to_owned(),
                source_idx,
                plan_version: crate::text_shape::MASK_PLAN_VERSION,
                plan_revision,
                candidate_bounds,
                reason: reason.to_owned(),
            });
        self.project.version = self.project.schema_version();
        self.flush()
    }

    /// Persist a prepared text-shaped plan before any engine runs. Raster
    /// artifacts are written before the manifest names them, matching the
    /// patch-buffer ordering used by [`complete_region`]. Revisions remain in
    /// the manifest so an undo snapshot can keep naming the plan that produced
    /// its patch while a newer preview is prepared for the same region.
    pub fn store_text_shape_plan(
        &mut self,
        source_idx: usize,
        plan: &MaskPlan,
        prepared: &PreparedMaskPlan,
    ) -> Result<(), StoreError> {
        let _write = self.begin_write()?;
        let source = self.project.sources.get(source_idx).ok_or_else(|| {
            StoreError::Malformed(format!(
                "text-shape plan source index {source_idx} is out of range"
            ))
        })?;
        if source.sha256 != plan.source_sha256 || prepared.identity.source_sha256 != source.sha256 {
            return Err(StoreError::StalePlan);
        }
        let canonical = plan
            .prepare(prepared.identity.page_w, prepared.identity.page_h)
            .map_err(|error| StoreError::Malformed(error.to_string()))?;
        if canonical != *prepared
            || plan.region_id != prepared.identity.region_id
            || plan.lower_composite_sha256 != prepared.identity.lower_composite_sha256
            || crate::text_shape::support_sha256(&prepared.write_support)
                != prepared.identity.support_sha256
        {
            return Err(StoreError::StalePlan);
        }
        if (prepared.identity.page_w,prepared.identity.page_h) != source.orientation.size(source.w,source.h) {
            return Err(StoreError::StalePlan);
        }
        if self
            .text_shape_correction(&plan.region_id)
            .is_some_and(|failed| {
                failed.source_idx != source_idx || failed.plan_revision >= plan.plan_revision
            })
        {
            return Err(StoreError::StalePlan);
        }

        let directory_key = sha256_hex(plan.region_id.as_bytes());
        let id_key = &prepared.identity.identity_sha256;
        if !id_key.bytes().all(|byte| byte.is_ascii_hexdigit()) || id_key.len() != 64 {
            return Err(StoreError::Malformed(
                "invalid text-shape plan identity digest".into(),
            ));
        }
        let prefix = format!(
            "mask-plans/{directory_key}/r{}-{}/",
            plan.plan_revision,
            &id_key[..16]
        );
        let record = TextShapePlanRecord {
            display_coordinates: source.orientation.0 != 1,
            region_id: plan.region_id.clone(),
            source_idx,
            geometry_policy: GeometryPolicy::TextShape,
            plan_version: plan.version,
            source_sha256: plan.source_sha256.clone(),
            lower_composite_sha256: plan.lower_composite_sha256.clone(),
            algorithm_id: plan.algorithm_id.clone(),
            model_id: plan.model_id.clone(),
            candidate_bounds: plan.candidate_bounds,
            refinement_crop: plan.refinement_crop,
            base_revision: plan.base_revision,
            correction_revision: plan.correction_revision,
            plan_revision: plan.plan_revision,
            padding_px: plan.padding_px,
            model_hole_margin_px: plan.model_hole_margin_px,
            reading_context: plan.reading_context,
            quality: plan.quality.clone(),
            identity: prepared.identity.clone(),
            base_mask_ref: format!("{prefix}base.mask"),
            additions_ref: format!("{prefix}additions.mask"),
            removals_ref: format!("{prefix}removals.mask"),
            write_support_ref: format!("{prefix}support.mask"),
            model_hole_ref: format!("{prefix}model-hole.mask"),
            blend_alpha_ref: plan
                .blend_alpha
                .as_ref()
                .map(|_| format!("{prefix}blend-alpha.mask")),
        };

        let existing = self.project.text_shape_plans.iter().find(|existing| {
            existing.same_identity(&plan.region_id, &prepared.identity.identity_sha256)
        });
        if let Some(existing) = existing {
            // Check before writing: a corrupted caller reusing an identity must
            // not overwrite a sidecar that an older patch still references.
            if existing != &record {
                return Err(StoreError::Malformed(
                    "a prepared plan identity was reused with different metadata".into(),
                ));
            }
            let (saved_plan, saved_prepared) = self.load_text_shape_plan_record(existing)?;
            if saved_plan != *plan || saved_prepared != *prepared {
                return Err(StoreError::Malformed(
                    "a prepared plan identity was reused with different raster inputs".into(),
                ));
            }
        } else if self.project.text_shape_plans.iter().any(|existing| {
            existing.region_id == plan.region_id && existing.plan_revision == plan.plan_revision
        }) {
            return Err(StoreError::Malformed(
                "a text-shape plan revision was reused".into(),
            ));
        }

        for (reference, raster, kind) in [
            (
                &record.base_mask_ref,
                &plan.base_mask,
                buffers::TextShapeMaskKind::Base,
            ),
            (
                &record.additions_ref,
                &plan.additions,
                buffers::TextShapeMaskKind::Additions,
            ),
            (
                &record.removals_ref,
                &plan.removals,
                buffers::TextShapeMaskKind::Removals,
            ),
            (
                &record.write_support_ref,
                &prepared.write_support,
                buffers::TextShapeMaskKind::Support,
            ),
            (
                &record.model_hole_ref,
                &prepared.model_hole,
                buffers::TextShapeMaskKind::ModelHole,
            ),
        ] {
            self.write_plan_mask(reference, raster, kind)?;
        }
        if let (Some(reference), Some(alpha)) = (&record.blend_alpha_ref, &plan.blend_alpha) {
            self.write_plan_mask(reference, alpha, buffers::TextShapeMaskKind::BlendAlpha)?;
        }

        if existing.is_none() {
            // Keep the row in append order and never discard the exact identity
            // referenced by an already committed patch or undo record.
            self.project.text_shape_plans.push(record.clone());
        }
        self.project
            .text_shape_corrections
            .retain(|failed| failed.region_id != plan.region_id);
        self.project.version = self.project.schema_version();
        // The same row may already exist; the just-encoded refs are stable and
        // identical, and flush is still useful when called as a retry after an
        // interrupted manifest write.
        if self.project.text_shape_plans.iter().any(|existing| {
            existing.same_identity(&plan.region_id, &prepared.identity.identity_sha256)
        }) {
            self.flush()?;
        }
        Ok(())
    }

    /// Load the most recent prepared plan for a stable region ID. The returned
    /// raster is verified against the persisted support artifact before it is
    /// trusted as preview authority.
    pub fn load_text_shape_plan(
        &self,
        region_id: &str,
    ) -> Result<Option<(MaskPlan, PreparedMaskPlan)>, StoreError> {
        let Some(record) = self
            .project
            .text_shape_plans
            .iter()
            .filter(|record| record.region_id == region_id)
            .max_by_key(|record| record.plan_revision)
        else {
            return Ok(None);
        };
        self.load_text_shape_plan_record(record).map(Some)
    }

    /// Load an exact plan revision by its prepared identity. Patches and undo
    /// snapshots use this instead of resolving the latest preview.
    pub fn load_text_shape_plan_identity(
        &self,
        region_id: &str,
        identity_sha256: &str,
    ) -> Result<Option<(MaskPlan, PreparedMaskPlan)>, StoreError> {
        let Some(record) = self
            .project
            .text_shape_plans
            .iter()
            .find(|record| record.same_identity(region_id, identity_sha256))
        else {
            return Ok(None);
        };
        self.load_text_shape_plan_record(record).map(Some)
    }

    /// Recheck the values captured by the preview/apply protocol immediately
    /// before rendering or attaching output.
    pub fn validate_text_shape_plan(
        &self,
        region_id: &str,
        expected_source_sha256: &str,
        expected_lower_composite_sha256: &str,
        expected_identity_sha256: &str,
        expected_support_sha256: &str,
    ) -> Result<PreparedMaskPlan, StoreError> {
        let (_, prepared) = self
            .load_text_shape_plan_identity(region_id, expected_identity_sha256)?
            .ok_or(StoreError::StalePlan)?;
        if prepared.identity.source_sha256 != expected_source_sha256
            || prepared.identity.lower_composite_sha256 != expected_lower_composite_sha256
            || prepared.identity.identity_sha256 != expected_identity_sha256
            || prepared.identity.support_sha256 != expected_support_sha256
        {
            return Err(StoreError::StalePlan);
        }
        Ok(prepared)
    }

    /// Commit a patch against one exact text-shape preview. This method is the
    /// only persistence path that marks a patch as `text_shape`; the legacy
    /// [`complete_region`] path keeps its prior behavior even if an unused
    /// prepared preview exists for a region with the same ID.
    pub fn complete_text_shape_region(
        &mut self,
        source_idx: usize,
        patch: &Patch,
        plan_identity_sha256: &str,
        review_state: Option<String>,
    ) -> Result<(), StoreError> {
        let (plan, prepared) = self
            .load_text_shape_plan_identity(&patch.id, plan_identity_sha256)?
            .ok_or(StoreError::StalePlan)?;
        let source = self
            .project
            .sources
            .get(source_idx)
            .ok_or(StoreError::StalePlan)?;
        if plan.region_id != patch.id
            || source_idx
                != self
                    .project
                    .text_shape_plans
                    .iter()
                    .find(|record| record.same_identity(&patch.id, plan_identity_sha256))
                    .map(|record| record.source_idx)
                    .unwrap_or(usize::MAX)
            || source.sha256 != plan.source_sha256
            || patch.mask.bounds != prepared.write_support.bounds
            || patch.mask.bits != prepared.write_support.bits
        {
            return Err(StoreError::StalePlan);
        }
        let mut patch = patch.clone();
        if !patch.provenance.params_snapshot.is_object() {
            patch.provenance.params_snapshot = serde_json::json!({});
        }
        patch.provenance.params_snapshot["geometry_policy"] = serde_json::json!("text_shape");
        patch.provenance.params_snapshot["write_support_sha256"] =
            serde_json::json!(prepared.identity.support_sha256);
        self.complete_region_with_policy(
            source_idx,
            &patch,
            review_state,
            GeometryPolicy::TextShape,
            Some(plan_identity_sha256.to_owned()),
            self.project.text_shape_plans.iter().find(|r|r.same_identity(&patch.id,plan_identity_sha256)).is_some_and(|r|r.display_coordinates),
        )
    }

    /// Select a previously committed text-shaped patch, preserving every
    /// revision for subsequent redo. The caller supplies the opaque content
    /// revision from a saved region snapshot (or an old plan identity).
    pub fn restore_text_shape_revision(
        &mut self,
        region_id: &str,
        revision_or_plan_identity: &str,
    ) -> Result<bool, StoreError> {
        let mut revisions = self
            .project
            .patches
            .iter()
            .chain(self.project.text_shape_patch_revisions.iter());
        let exact = revisions.clone().find(|record| {
            record.id == region_id
                && record.text_shape_revision_id().as_deref() == Some(revision_or_plan_identity)
        });
        let Some(selected) = exact
            .or_else(|| {
                revisions.find(|record| {
                    record.id == region_id
                        && record.geometry_policy == GeometryPolicy::TextShape
                        && record.text_shape_plan_identity.as_deref()
                            == Some(revision_or_plan_identity)
                })
            })
            .cloned()
        else {
            return Ok(false);
        };
        let identity = selected
            .text_shape_plan_identity
            .as_deref()
            .ok_or(StoreError::StalePlan)?;
        let (_, prepared) = self
            .load_text_shape_plan_identity(region_id, identity)?
            .ok_or(StoreError::StalePlan)?;
        let selected_patch = self.load_patch(&selected)?;
        if selected_patch.mask.bounds != prepared.write_support.bounds
            || selected_patch.mask.bits != prepared.write_support.bits
        {
            return Err(StoreError::StalePlan);
        }
        let Some(current) = self
            .project
            .patches
            .iter_mut()
            .find(|record| record.id == region_id)
        else {
            return Ok(false);
        };
        if *current != selected {
            *current = selected;
            self.flush()?;
        }
        Ok(true)
    }

    pub fn restore_legacy_revision(
        &mut self,
        region_id: &str,
        revision: &str,
    ) -> Result<bool, StoreError> {
        let _write = self.begin_write()?;
        let selected = self.project.patches.iter()
            .chain(self.project.legacy_patch_revisions.iter())
            .find(|row| row.id == region_id && row.legacy_revision_id().as_deref() == Some(revision))
            .cloned();
        let Some(selected) = selected else { return Ok(false) };
        let patch = self.load_patch(&selected)?;
        buffers::write_atomic(
            &self.dir.join(format!("{region_id}.ink")),
            &buffers::encode_mask(&patch.ink),
        )?;
        let Some(current) = self.project.patches.iter_mut().find(|row| row.id == region_id) else {
            return Ok(false);
        };
        if *current != selected {
            *current = selected;
            self.flush()?;
        }
        Ok(true)
    }

    fn write_plan_mask(
        &self,
        reference: &str,
        raster: &MaskRaster,
        kind: buffers::TextShapeMaskKind,
    ) -> Result<(), StoreError> {
        let path = self.plan_artifact_path(reference)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| StoreError::io(parent, error))?;
        }
        let bytes = buffers::encode_text_shape_mask_parts(raster.bounds, &raster.bits, kind)?;
        buffers::write_atomic(&path, &bytes)
    }

    fn read_plan_mask(
        &self,
        reference: &str,
        kind: buffers::TextShapeMaskKind,
    ) -> Result<MaskRaster, StoreError> {
        let path = self.plan_artifact_path(reference)?;
        let bytes = std::fs::read(&path).map_err(|error| StoreError::io(&path, error))?;
        Ok(buffers::decode_text_shape_mask(&bytes, kind)?.into())
    }

    fn plan_artifact_path(&self, reference: &str) -> Result<PathBuf, StoreError> {
        let relative = Path::new(reference);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(StoreError::Malformed(
                "unsafe text-shape artifact reference".into(),
            ));
        }
        Ok(self.dir.join(relative))
    }

    fn load_text_shape_plan_record(
        &self,
        record: &TextShapePlanRecord,
    ) -> Result<(MaskPlan, PreparedMaskPlan), StoreError> {
        let source = self
            .project
            .sources
            .get(record.source_idx)
            .ok_or(StoreError::StalePlan)?;
        if record.geometry_policy != GeometryPolicy::TextShape
            || record.region_id != record.identity.region_id
            || source.sha256 != record.source_sha256
            || record.source_sha256 != record.identity.source_sha256
            || record.lower_composite_sha256 != record.identity.lower_composite_sha256
        {
            return Err(StoreError::StalePlan);
        }
        let (width,height) = if record.display_coordinates { source.orientation.size(source.w,source.h) } else { (source.w,source.h) };
        let plan = MaskPlan {
            version: record.plan_version,
            region_id: record.region_id.clone(),
            source_sha256: record.source_sha256.clone(),
            lower_composite_sha256: record.lower_composite_sha256.clone(),
            algorithm_id: record.algorithm_id.clone(),
            model_id: record.model_id.clone(),
            candidate_bounds: record.candidate_bounds,
            refinement_crop: record.refinement_crop,
            base_revision: record.base_revision,
            correction_revision: record.correction_revision,
            plan_revision: record.plan_revision,
            base_mask: self
                .read_plan_mask(&record.base_mask_ref, buffers::TextShapeMaskKind::Base)?,
            additions: self
                .read_plan_mask(&record.additions_ref, buffers::TextShapeMaskKind::Additions)?,
            removals: self
                .read_plan_mask(&record.removals_ref, buffers::TextShapeMaskKind::Removals)?,
            padding_px: record.padding_px,
            model_hole_margin_px: record.model_hole_margin_px,
            reading_context: record.reading_context,
            blend_alpha: record
                .blend_alpha_ref
                .as_deref()
                .map(|reference| {
                    self.read_plan_mask(reference, buffers::TextShapeMaskKind::BlendAlpha)
                })
                .transpose()?,
            quality: record.quality.clone(),
        };
        let prepared = plan
            .prepare(width, height)
            .map_err(|error| StoreError::Malformed(error.to_string()))?;
        let persisted_support = self.read_plan_mask(
            &record.write_support_ref,
            buffers::TextShapeMaskKind::Support,
        )?;
        let persisted_hole = self.read_plan_mask(
            &record.model_hole_ref,
            buffers::TextShapeMaskKind::ModelHole,
        )?;
        if prepared.identity != record.identity
            || prepared.write_support != persisted_support
            || prepared.model_hole != persisted_hole
            || crate::text_shape::support_sha256(&persisted_support)
                != record.identity.support_sha256
        {
            return Err(StoreError::StalePlan);
        }
        prepared
            .verify_against(&plan, width, height)
            .map_err(|_| StoreError::StalePlan)?;
        Ok((plan, prepared))
    }

    #[cfg(test)]
    fn plan_fixture(&self, region_id: &str, plan_revision: u64, padding_px: u32) -> MaskPlan {
        let source = &self.project.sources[0];
        let base_bounds = Rect::new(10, 10, 3, 2);
        let mut base = crate::mask::Mask::empty(base_bounds);
        base.set(10, 10, true);
        base.set(12, 11, true);
        let empty = crate::mask::Mask::empty(Rect::new(0, 0, 0, 0));
        MaskPlan {
            version: crate::text_shape::MASK_PLAN_VERSION,
            region_id: region_id.to_owned(),
            source_sha256: source.sha256.clone(),
            lower_composite_sha256: format!("underlay-{plan_revision}"),
            algorithm_id: "synthetic-test".into(),
            model_id: None,
            candidate_bounds: Rect::new(
                6,
                6,
                source.w.saturating_sub(6).min(12),
                source.h.saturating_sub(6).min(12),
            ),
            refinement_crop: Rect::new(
                5,
                5,
                source.w.saturating_sub(5).min(14),
                source.h.saturating_sub(5).min(14),
            ),
            base_revision: 1,
            correction_revision: 1,
            plan_revision,
            base_mask: base.into(),
            additions: empty.clone().into(),
            removals: empty.into(),
            padding_px,
            model_hole_margin_px: 2,
            reading_context: Rect::new(0, 0, source.w, source.h),
            blend_alpha: Some(MaskRaster {
                bounds: base_bounds,
                bits: vec![64, 0, 0, 0, 0, 192],
            }),
            quality: MaskQualityState::Ready,
        }
    }

    #[cfg(test)]
    fn patch_with_support(id: &str, support: &MaskRaster) -> Patch {
        let mut pixels = crate::image::fixtures::by_name("l8").raster;
        pixels.width = support.bounds.w;
        pixels.height = support.bounds.h;
        pixels.data = vec![127; support.bounds.w as usize * support.bounds.h as usize];
        Patch {
            id: id.to_owned(),
            mask: support.to_mask(),
            ink: support.to_mask(),
            pixels,
            order: 3,
            visible: true,
            provenance: Provenance {
                engine: Engine::Fill,
                engine_version: "test".into(),
                model_sha256: None,
                execution_provider: "cpu".into(),
                params_snapshot: serde_json::Value::Null,
                mask_sha256: "0".repeat(64),
                source_sha256: "1".repeat(64),
                cloud: None,
                created: 0,
            },
        }
    }

    /// Write a text-group evidence file a patch about to be recorded names
    /// (`evidence/<sha256>.mtev`), once. No manifest write: the row that names
    /// it follows.
    ///
    /// The write discipline of every sidecar: the job's lock and the stale
    /// check first. Content addressing already means a stale job cannot
    /// replace a named file with other bytes (an existing name is never
    /// rewritten); the lock is what orders the write against the open-time
    /// sweep of unnamed evidence ([`Job::open`]). That sweep runs only while
    /// the job's lock is free, so the caller holds the job from this write to
    /// the flush that names the file, as a run does for its whole chapter.
    pub fn write_evidence(&self, evidence: &crate::text_groups::EvidenceBlob) -> Result<(), StoreError> {
        let _write = self.begin_write()?;
        write_evidence(&self.dir, evidence)
    }

    /// Record one completed region: its pixels, its mask, its provenance - and
    /// flush.
    ///
    /// §6's "flushed after every completed region" is this method's whole
    /// reason for existing, and the order matters: the buffers are on disk
    /// before the manifest names them, so a crash between the two leaves an
    /// orphaned buffer (swept at launch) rather than a manifest pointing at
    /// nothing.
    pub fn complete_region(
        &mut self,
        source_idx: usize,
        patch: &Patch,
        review_state: Option<String>,
    ) -> Result<(), StoreError> {
        self.complete_region_with_policy(
            source_idx,
            patch,
            review_state,
            GeometryPolicy::Legacy,
            None,
            false,
        )
    }

    fn complete_region_with_policy(
        &mut self,
        source_idx: usize,
        patch: &Patch,
        review_state: Option<String>,
        geometry_policy: GeometryPolicy,
        text_shape_plan_identity: Option<String>,
        display_coordinates: bool,
    ) -> Result<(), StoreError> {
        let _write = self.begin_write()?;
        let record = PatchRecord::of(source_idx, patch, review_state);
        let mut record = PatchRecord {
            geometry_policy,
            text_shape_plan_identity,
            ..record
        };
        if display_coordinates { record.native_parts = self.store_native_parts(source_idx, patch)?; }
        if self.project.sources.iter().any(|source|source.orientation.0!=1) { record.native_order = self.project.strip.order.clone(); }
        if let Some(previous) = self.project.patches.iter().find(|r| r.id == record.id) { record.extra = previous.extra.clone(); }
        let mask_bytes = buffers::encode_mask(&patch.mask);
        let ink_bytes = buffers::encode_mask(&patch.ink);
        let pixel_bytes = buffers::encode_patch(&patch.pixels);
        if geometry_policy == GeometryPolicy::TextShape || geometry_policy == GeometryPolicy::Legacy {
            let mut hash = Sha256::new();
            hash.update(b"manga-cleaner/patch-revision/v1\0");
            for bytes in [&mask_bytes, &ink_bytes, &pixel_bytes] {
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
            hash.update(
                serde_json::to_vec(&record).map_err(|error| StoreError::Json(error.to_string()))?,
            );
            let revision = format!("{:x}", hash.finalize());
            let region_key = sha256_hex(patch.id.as_bytes());
            let stem = format!("patch-revisions/{region_key}/{revision}");
            record.mask_ref = format!("{stem}.mask");
            record.buffer_ref = format!("{stem}.buf");
        }
        for reference in [&record.mask_ref, &record.buffer_ref] {
            let path = self.dir.join(reference);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| StoreError::io(parent, error))?;
            }
        }
        buffers::write_atomic(&self.dir.join(&record.mask_ref), &mask_bytes)?;
        buffers::write_atomic(&self.dir.join(record.ink_ref()), &ink_bytes)?;
        buffers::write_atomic(&self.dir.join(&record.buffer_ref), &pixel_bytes)?;

        // A re-run replaces the record rather than appending a second one with
        // the same id - §3 forbids overwriting a prior *provenance* entry, and
        // that history lives in the mask revisions, not in duplicate rows here.
        if let Some(previous) = self
            .project
            .patches
            .iter()
            .find(|existing| existing.id == record.id)
            .cloned()
        {
            if previous.geometry_policy == GeometryPolicy::TextShape
                && previous != record
                && !self.project.text_shape_patch_revisions.contains(&previous)
            {
                self.project
                    .text_shape_patch_revisions
                    .push(previous.clone());
            }
            if previous.geometry_policy == GeometryPolicy::Legacy && previous != record {
                let archived = if previous.legacy_revision_id().is_some() {
                    previous
                } else {
                    // A v3 row used mutable sidecar names. Preserve its bytes
                    // before the next revision can replace those names.
                    let old = self.load_patch(&previous)?;
                    let old_mask = buffers::encode_mask(&old.mask);
                    let old_ink = buffers::encode_mask(&old.ink);
                    let old_pixels = buffers::encode_patch(&old.pixels);
                    let mut hash = Sha256::new();
                    hash.update(b"manga-cleaner/patch-revision/v1\0");
                    for bytes in [&old_mask, &old_ink, &old_pixels] {
                        hash.update((bytes.len() as u64).to_le_bytes());
                        hash.update(bytes);
                    }
                    hash.update(serde_json::to_vec(&previous).map_err(|error| StoreError::Json(error.to_string()))?);
                    let stem = format!("patch-revisions/{}/{:x}", sha256_hex(previous.id.as_bytes()), hash.finalize());
                    let mut archived = previous;
                    archived.mask_ref = format!("{stem}.mask");
                    archived.buffer_ref = format!("{stem}.buf");
                    let parent = self.dir.join("patch-revisions").join(sha256_hex(archived.id.as_bytes()));
                    std::fs::create_dir_all(&parent).map_err(|error| StoreError::io(&parent, error))?;
                    buffers::write_atomic(&self.dir.join(&archived.mask_ref), &old_mask)?;
                    buffers::write_atomic(&self.dir.join(archived.ink_ref()), &old_ink)?;
                    buffers::write_atomic(&self.dir.join(&archived.buffer_ref), &old_pixels)?;
                    archived
                };
                if !self.project.legacy_patch_revisions.contains(&archived) {
                    self.project.legacy_patch_revisions.push(archived);
                }
            }
        }
        if geometry_policy == GeometryPolicy::TextShape
            && !self.project.text_shape_patch_revisions.contains(&record)
        {
            self.project.text_shape_patch_revisions.push(record.clone());
        }
        if geometry_policy == GeometryPolicy::Legacy
            && !self.project.legacy_patch_revisions.contains(&record)
        {
            self.project.legacy_patch_revisions.push(record.clone());
        }
        if geometry_policy == GeometryPolicy::Legacy {
            // The v3 reader derives <id>.ink even for an unfamiliar buffer
            // reference. Keep that compatibility sidecar at the active ink.
            buffers::write_atomic(&self.dir.join(format!("{}.ink", record.id)), &ink_bytes)?;
        }
        match self
            .project
            .patches
            .iter_mut()
            .find(|existing| existing.id == record.id)
        {
            Some(existing) => *existing = record,
            None => self.project.patches.push(record),
        }
        if geometry_policy == GeometryPolicy::TextShape {
            self.project.version = self.project.schema_version();
        }
        self.flush()
    }

    /// Record a region that was deliberately left alone, and flush.
    pub fn leave_untouched(
        &mut self,
        source_idx: usize,
        bbox: Rect,
        reason: &str,
    ) -> Result<(), StoreError> {
        self.leave_untouched_rows(vec![RegionUntouched {
            source_idx,
            bbox,
            reason: reason.to_owned(),
            inside_bubble: None,
            lettering: None,
        }])
    }

    /// Record a held text-group candidate, and flush. An untouched row like
    /// [`Job::leave_untouched`]'s, carrying where grouping found it. A run
    /// holds its candidates with their lettering through
    /// [`Job::leave_untouched_rows`].
    pub fn hold_candidate(
        &mut self,
        source_idx: usize,
        bbox: Rect,
        reason: &str,
        inside_bubble: bool,
    ) -> Result<(), StoreError> {
        self.leave_untouched_rows(vec![RegionUntouched {
            source_idx,
            bbox,
            reason: reason.to_owned(),
            inside_bubble: Some(inside_bubble),
            lettering: None,
        }])
    }

    /// Record a page's regions that were deliberately left alone, in one
    /// flush. No write at all for none. A held candidate's lettering is
    /// written before the manifest that names it.
    pub fn leave_untouched_rows(
        &mut self,
        rows: Vec<impl Into<PendingUntouched>>,
    ) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let _write = self.begin_write()?;
        let _detection_io = detection_io().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let rows = self.write_held_lettering(rows)?;
        self.project.regions_untouched.extend(rows);
        self.flush()
    }

    /// Write each held candidate's lettering to `candidates/` and name it on
    /// its row. Called under the job's write and the sweep's mutex, before the
    /// flush that names the files, the order [`Job::store_detection`] keeps.
    fn write_held_lettering(
        &self,
        rows: Vec<impl Into<PendingUntouched>>,
    ) -> Result<Vec<RegionUntouched>, StoreError> {
        let mut written = Vec::with_capacity(rows.len());
        for pending in rows {
            let PendingUntouched { mut row, lettering } = pending.into();
            if let Some(source) = self.project.sources.get(row.source_idx) {
                let (w,h) = source.orientation.size(source.w,source.h);
                row.bbox = source.orientation.inverse().rect(row.bbox,w,h);
            }
            if let Some((mask, scale)) = lettering.filter(|(mask, _)| !mask.is_empty()) {
                let mask = self.native_detection_mask(row.source_idx, &mask);
                let bytes = buffers::encode_mask(&mask);
                let mask_ref = format!("candidates/{}.mask", sha256_hex(&bytes));
                let dir = self.dir.join("candidates");
                std::fs::create_dir_all(&dir).map_err(|error| StoreError::io(&dir, error))?;
                buffers::write_atomic(&self.dir.join(&mask_ref), &bytes)?;
                row.lettering = Some(HeldLettering { mask_ref, scale });
            }
            written.push(row);
        }
        Ok(written)
    }

    /// A held candidate's lettering and the scale it was found at, in page
    /// pixels. `None` for a row that kept none, or whose file is gone, fails
    /// its digest or does not decode: the caller then looks under the box, as
    /// it did before rows kept lettering. The last two say so in the log.
    pub fn load_held_lettering(
        &self,
        row: &RegionUntouched,
    ) -> Result<Option<(crate::mask::Mask, f32)>, StoreError> {
        let Some(held) = row.lettering.as_ref() else {
            return Ok(None);
        };
        let Some(digest) = held.mask_ref.strip_prefix("candidates/").and_then(|name| name.strip_suffix(".mask"))
            .filter(|digest| !digest.contains(['/', '\\']) && !digest.contains(".."))
        else {
            return Err(StoreError::Malformed(format!("held lettering {} is outside candidates/", held.mask_ref)));
        };
        let path = self.dir.join(&held.mask_ref);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StoreError::io(&path, error)),
        };
        if sha256_hex(&bytes) != digest {
            eprintln!("manga-cleaner: held lettering {} fails its digest; looking under the box", held.mask_ref);
            return Ok(None);
        }
        match buffers::decode_mask(&bytes) {
            Ok(mask) => Ok(Some((self.display_detection_mask(row.source_idx, &mask), held.scale))),
            Err(error) => {
                eprintln!("manga-cleaner: held lettering {} does not decode ({error}); looking under the box",
                    held.mask_ref);
                Ok(None)
            }
        }
    }

    /* ---------- detected regions ---------- */

    /// Mint the next detection id on a page, `{page_id}-d{n}`. Moves the
    /// counter and does not flush: the id is written with the record that
    /// uses it, by [`Job::store_detection`].
    pub fn try_next_detection_id(&mut self, page_id: &str) -> Result<String, StoreError> {
        let n = self.project.counters.detections;
        self.project.counters.detections = n.checked_add(1)
            .ok_or_else(|| StoreError::Malformed("detection id counter exhausted".into()))?;
        Ok(format!("{page_id}-d{n}"))
    }

    /// Compatibility caller for existing fixtures. Production writes use the
    /// fallible allocator so exhaustion leaves the manifest unchanged.
    pub fn next_detection_id(&mut self, page_id: &str) -> String {
        self.try_next_detection_id(page_id).expect("detection id counter exhausted")
    }

    /// Whether a detection over `bbox` lies at least half inside an existing
    /// patch's box on this source: text that was cleaned already, by an
    /// earlier run or by hand, and must not be stored to be cleaned twice.
    ///
    /// Every patch row counts, a deleted one included. A mask the user deleted
    /// is text they chose to keep, and a detection over it would bring the
    /// edit back through the next Clean.
    pub fn detection_is_cleaned_already(&self, source_idx: usize, bbox: Rect) -> bool {
        self.project
            .patches
            .iter()
            .filter(|record| record.source_idx == source_idx)
            .any(|record| half_inside(bbox, orientation::display_bbox(&self.project, record)))
    }

    /// Store one detection and flush: its two masks first, then the record,
    /// the same order [`Job::complete_region`] keeps and for the same reason.
    ///
    /// `mask_ref` is set here, so a caller cannot name a file outside the
    /// detections directory. A record with the id of one already stored
    /// replaces it.
    pub fn store_detection(
        &mut self,
        mut record: DetectedRegion,
        mask: &crate::mask::Mask,
        ink: &crate::mask::Mask,
    ) -> Result<(), StoreError> {
        // The job before the sweep's mutex, always: `open` takes them the other
        // way round and only ever tries the job, so the two cannot deadlock.
        let _write = self.begin_write()?;
        let _detection_io = detection_io().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        record.mask_ref = format!("detections/{}.mask", record.id);
        let dir = self.dir.join("detections");
        std::fs::create_dir_all(&dir).map_err(|error| StoreError::io(&dir, error))?;
        self.write_detection_files(&mut record, mask, ink, None)?;
        match self.project.detections.iter_mut().find(|row| row.id == record.id) {
            Some(existing) => *existing = record,
            None => self.project.detections.push(record),
        }
        self.flush()
    }

    /// One stored detection and its masks, in page pixels. `None` for an id
    /// no detection carries.
    ///
    /// A missing lettering file reads as the mask, the answer
    /// [`Job::load_patch`] gives a patch written before its ink file existed.
    pub fn load_detection(&self, id: &str) -> Result<Option<LoadedDetection>, StoreError> {
        let Some(record) = self.project.detections.iter().find(|row| row.id == id) else {
            return Ok(None);
        };
        let mask_path = self.dir.join(&record.mask_ref);
        let mask_bytes = std::fs::read(&mask_path).map_err(|e| StoreError::io(&mask_path, e))?;
        if !record.mask_sha256.is_empty() && sha256_hex(&mask_bytes) != record.mask_sha256 {
            return Err(StoreError::Malformed(format!("detection {} mask digest mismatch", record.id)));
        }
        let mask = buffers::decode_mask(&mask_bytes)?;
        let ink_path = self.dir.join(record.ink_ref());
        let ink = match std::fs::read(&ink_path) {
            Ok(bytes) => buffers::decode_mask(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && record.mask_sha256.is_empty() => mask.clone(),
            Err(e) => return Err(StoreError::io(&ink_path, e)),
        };
        let mut displayed = record.clone();
        if let Some(source) = self.project.sources.get(record.source_idx) {
            displayed.bbox = source.orientation.rect(record.bbox, source.w, source.h);
        }
        Ok(Some(LoadedDetection { record: displayed,
            mask: self.display_detection_mask(record.source_idx, &mask),
            ink: self.display_detection_mask(record.source_idx, &ink), evidence: None, base: None }))
    }

    /// One stored detection's mask and lettering before its padding grew
    /// them: the stored pair itself while the padding is 0. `None` for an id
    /// no detection carries.
    pub fn load_detection_base(&self, id: &str) -> Result<Option<(crate::mask::Mask, crate::mask::Mask)>, StoreError> {
        let Some(found) = self.load_detection(id)? else {
            return Ok(None);
        };
        if found.record.padding_px == 0 {
            return Ok(Some((found.mask, found.ink)));
        }
        let read = |reference: String| -> Result<crate::mask::Mask, StoreError> {
            let path = self.dir.join(reference);
            Ok(self.display_detection_mask(found.record.source_idx, &buffers::decode_mask(&std::fs::read(&path).map_err(|e| StoreError::io(&path, e))?)?))
        };
        Ok(Some((read(found.record.base_mask_ref())?, read(found.record.base_ink_ref())?)))
    }

    /// Write one detection's sidecar files under the names its record already
    /// carries, and record the mask's digest: the mask, the lettering, and
    /// their ungrown pair when the record is padded. A padded record without
    /// that pair is refused, because its padding could never be changed again.
    fn native_detection_mask(&self, source_idx: usize, mask: &crate::mask::Mask) -> crate::mask::Mask {
        self.project.sources.get(source_idx).map_or_else(|| mask.clone(), |source| {
            let (w,h) = source.orientation.size(source.w,source.h);
            source.orientation.inverse().mask(mask,w,h)
        })
    }
    fn display_detection_mask(&self, source_idx: usize, mask: &crate::mask::Mask) -> crate::mask::Mask {
        self.project.sources.get(source_idx).map_or_else(|| mask.clone(), |source| source.orientation.mask(mask,source.w,source.h))
    }

    fn write_detection_files(
        &self,
        record: &mut DetectedRegion,
        mask: &crate::mask::Mask,
        ink: &crate::mask::Mask,
        base: Option<&(crate::mask::Mask, crate::mask::Mask)>,
    ) -> Result<(), StoreError> {
        let source_idx = record.source_idx;
        let mask = self.native_detection_mask(source_idx, mask);
        let ink = self.native_detection_mask(source_idx, ink);
        if let Some(source) = self.project.sources.get(source_idx) {
            let (w,h) = source.orientation.size(source.w,source.h);
            record.bbox = source.orientation.inverse().rect(record.bbox,w,h);
        }
        let mask_bytes = buffers::encode_mask(&mask);
        record.mask_sha256 = sha256_hex(&mask_bytes);
        if record.padding_px > 0 {
            let (base_mask, base_ink) = base.ok_or_else(|| {
                StoreError::Malformed(format!("detection {} is padded without its unpadded masks", record.id))
            })?;
            buffers::write_atomic(&self.dir.join(record.base_mask_ref()), &buffers::encode_mask(&self.native_detection_mask(source_idx, base_mask)))?;
            buffers::write_atomic(&self.dir.join(record.base_ink_ref()), &buffers::encode_mask(&self.native_detection_mask(source_idx, base_ink)))?;
        }
        buffers::write_atomic(&self.dir.join(&record.mask_ref), &mask_bytes)?;
        buffers::write_atomic(&self.dir.join(record.ink_ref()), &buffers::encode_mask(&ink))
    }

    /// Replace a page's Detect results in one manifest write. Sidecars are
    /// written first under fresh ids; old sidecars remain until the new
    /// manifest is durable. An unsuccessful write leaves the old rows intact.
    /// The page is recorded in `detected` in the same write.
    pub fn replace_page_detection_results(
        &mut self, source_idx: usize, page_id: &str,
        detections: Vec<LoadedDetection>, untouched: Vec<impl Into<PendingUntouched>>,
    ) -> Result<(), StoreError> {
        self.save_page_detection_results(source_idx, page_id, detections, untouched, true, true)
    }

    /// Add a cloud Detect pass without replacing existing masks or edits.
    /// Fresh ids come from the durable allocator, including after deletions.
    pub fn append_page_detection_results(
        &mut self, source_idx: usize, page_id: &str,
        detections: Vec<LoadedDetection>, untouched: Vec<impl Into<PendingUntouched>>,
    ) -> Result<(), StoreError> {
        self.save_page_detection_results(source_idx, page_id, detections, untouched, false, true)
    }

    /// Add what a Detect of one area of a page found to what the page holds.
    ///
    /// Nothing stored is replaced or moved. The detections are the caller's
    /// to have cut to what the page does not hold already, by pixel. An
    /// untouched row has no pixels to cut, so it is left out when its box lies
    /// at least half inside a row or detection the page already holds, or
    /// holds one at least half. The detections take orders after everything
    /// on the page, in the order given. The page is not recorded in
    /// `detected`, because an area is not the page. Answers how many
    /// detections were stored.
    pub fn add_area_detection_results(
        &mut self, source_idx: usize, page_id: &str,
        detections: Vec<LoadedDetection>, untouched: Vec<impl Into<PendingUntouched>>,
    ) -> Result<usize, StoreError> {
        // Stored boxes are in the source's own orientation, new ones in the
        // page's as it is shown.
        let shown = |bbox: Rect| self.project.sources.get(source_idx)
            .map_or(bbox, |source| source.orientation.rect(bbox, source.w, source.h));
        let same = |a: Rect, b: Rect| half_inside(a, b) || half_inside(b, a);
        let rows: Vec<Rect> = self.project.regions_untouched.iter()
            .filter(|row| row.source_idx == source_idx)
            .map(|row| row.bbox)
            .chain(self.project.detections.iter()
                .filter(|row| row.source_idx == source_idx)
                .map(|row| row.bbox))
            .map(shown)
            .collect();
        let mut order = self.project.detections.iter()
            .filter(|row| row.source_idx == source_idx)
            .map(|row| row.order)
            .chain(self.project.patches.iter()
                .filter(|record| record.source_idx == source_idx)
                .map(|record| record.order))
            .max()
            .map_or(0, |order| order.saturating_add(1));
        let detections: Vec<LoadedDetection> = detections.into_iter()
            .map(|mut found| {
                found.record.order = order;
                order = order.saturating_add(1);
                found
            })
            .collect();
        let untouched: Vec<PendingUntouched> = untouched.into_iter().map(Into::into)
            .filter(|pending| !rows.iter().any(|kept| same(pending.row.bbox, *kept)))
            .collect();
        let stored = detections.len();
        if stored == 0 && untouched.is_empty() {
            return Ok(0);
        }
        self.save_page_detection_results(source_idx, page_id, detections, untouched, false, false)?;
        Ok(stored)
    }

    fn save_page_detection_results(
        &mut self, source_idx: usize, page_id: &str,
        detections: Vec<LoadedDetection>, untouched: Vec<impl Into<PendingUntouched>>, replace: bool,
        whole_page: bool,
    ) -> Result<(), StoreError> {
        let _write = self.begin_write()?;
        let _detection_io = detection_io().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = self.project.clone();
        let dir = self.dir.join("detections");
        std::fs::create_dir_all(&dir).map_err(|error| StoreError::io(&dir, error))?;
        let result = (|| {
            let mut records = Vec::new();
            for mut found in detections {
                found.record.id = self.try_next_detection_id(page_id)?;
                found.record.source_idx = source_idx;
                found.record.mask_ref = format!("detections/{}.mask", found.record.id);
                if let Some(evidence) = &found.evidence {
                    write_evidence(&self.dir, evidence)?;
                }
                self.write_detection_files(&mut found.record, &found.mask, &found.ink, found.base.as_ref())?;
                records.push(found.record);
            }
            let untouched = self.write_held_lettering(untouched)?;
            let old = if replace { self.take_page_detections(source_idx) } else { Vec::new() };
            let mut gate_dropped = 0u32;
            let mut declined = 0u32;
            self.project.regions_untouched.retain(|row| {
                if !replace || row.source_idx != source_idx { return true; }
                // A held candidate (it records its balloon) was counted as
                // neither, so it leaves neither count.
                if row.inside_bubble.is_some() {
                } else if row.reason.contains("gateSkipped") || matches!(row.reason.as_str(),
                    "review.reason.languageSkipped" | "review.reason.outsideLanguageUnverified") {
                    gate_dropped += 1;
                } else { declined += 1; }
                false
            });
            self.project.counters.gate_dropped = self.project.counters.gate_dropped.saturating_sub(gate_dropped);
            self.project.counters.declined = self.project.counters.declined.saturating_sub(declined);
            for row in &untouched {
                // A held candidate refused nothing: no count.
                if row.inside_bubble.is_some() {
                } else if row.reason.contains("gateSkipped") || matches!(row.reason.as_str(),
                    "review.reason.languageSkipped" | "review.reason.outsideLanguageUnverified") {
                    self.project.counters.gate_dropped += 1;
                } else { self.project.counters.declined += 1; }
            }
            self.project.regions_untouched.extend(untouched);
            self.project.detections.extend(records);
            self.clear_errored(source_idx);
            if whole_page && !self.project.detected.contains(&source_idx) {
                self.project.detected.push(source_idx);
            }
            self.flush()?;
            self.delete_detection_files(&old);
            Ok(())
        })();
        if result.is_err() { self.project = before; }
        result
    }

    /// A hand edit of a page's detection masks, in one manifest write.
    ///
    /// `updated` keep their ids and take the caller's record otherwise; their
    /// masks go to fresh files (`detections/{id}.e{n}.mask` and its `.ink`), so
    /// the files the manifest on disk names are never written over, and a
    /// failed write leaves every one of them as it was. `created` get ids
    /// minted here. `removed` leave the manifest. The superseded and removed
    /// files are deleted only once the new manifest is durable, the order
    /// [`Job::replace_page_detection_results`] keeps. On any error the job is
    /// restored and the manifest is untouched; files already written are
    /// orphans the next open sweeps.
    ///
    /// Every id named must be a detection on `source_idx`, and at most once.
    /// Answers the created ids, in the order given.
    pub fn rewrite_page_detections(
        &mut self, source_idx: usize, page_id: &str,
        updated: Vec<LoadedDetection>, created: Vec<LoadedDetection>, removed: &[String],
    ) -> Result<Vec<String>, StoreError> {
        let _write = self.begin_write()?;
        let _detection_io = detection_io().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = self.project.clone();
        let dir = self.dir.join("detections");
        std::fs::create_dir_all(&dir).map_err(|error| StoreError::io(&dir, error))?;
        let result = (|| {
            let mut named = std::collections::HashSet::new();
            let on_page = |project: &Project, id: &str| project.detections.iter()
                .position(|row| row.id == id && row.source_idx == source_idx);
            let mut superseded = Vec::new();
            for mut found in updated {
                let id = found.record.id.clone();
                let at = on_page(&self.project, &id).filter(|_| named.insert(id.clone()))
                    .ok_or_else(|| StoreError::Malformed(format!("detection {id} is not an edit target on this page")))?;
                let old = self.project.detections[at].clone();
                // One past the edit number the current file carries, so the new
                // name is never the one the manifest on disk still reads.
                let edit = old.edits().saturating_add(1);
                found.record.source_idx = source_idx;
                found.record.mask_ref = format!("detections/{id}.e{edit}.mask");
                self.write_detection_files(&mut found.record, &found.mask, &found.ink, found.base.as_ref())?;
                self.project.detections[at] = found.record;
                superseded.push(old);
            }
            let mut ids = Vec::new();
            for mut found in created {
                found.record.id = self.try_next_detection_id(page_id)?;
                found.record.source_idx = source_idx;
                found.record.mask_ref = format!("detections/{}.mask", found.record.id);
                if let Some(evidence) = &found.evidence {
                    write_evidence(&self.dir, evidence)?;
                }
                self.write_detection_files(&mut found.record, &found.mask, &found.ink, found.base.as_ref())?;
                ids.push(found.record.id.clone());
                self.project.detections.push(found.record);
            }
            for id in removed {
                let at = on_page(&self.project, id).filter(|_| named.insert(id.clone()))
                    .ok_or_else(|| StoreError::Malformed(format!("detection {id} is not an edit target on this page")))?;
                superseded.push(self.project.detections.remove(at));
            }
            self.flush()?;
            self.delete_detection_files(&superseded);
            Ok(ids)
        })();
        if result.is_err() { self.project = before; }
        result
    }

    /// A crash may leave files written before the manifest flush. Only sweep
    /// the dedicated detection directory, and only ordinary mask and ink files.
    fn sweep_detection_files(&self) -> Result<(), StoreError> {
        let dir = self.dir.join("detections");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StoreError::io(&dir, error)),
        };
        let named: std::collections::HashSet<_> = self.project.detections.iter()
            .flat_map(DetectedRegion::files)
            .collect();
        for entry in entries {
            let entry = entry.map_err(|error| StoreError::io(&dir, error))?;
            let path = entry.path();
            if !entry.file_type().map_err(|error| StoreError::io(&path, error))?.is_file() { continue; }
            if !matches!(path.extension().and_then(|ext| ext.to_str()), Some("mask" | "ink")) { continue; }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue };
            if !named.contains(&format!("detections/{name}")) {
                std::fs::remove_file(&path).map_err(|error| StoreError::io(&path, error))?;
            }
        }
        Ok(())
    }

    /// Delete held candidates' lettering (`candidates/*.mask`) no untouched
    /// row names. A row leaves the manifest by many paths (a clean, a delete,
    /// a page detected again) and none of them owns its file, which one file
    /// shared by two rows could not have anyway, so it goes here, at open,
    /// under the job's lock, like the detection sweep.
    fn sweep_held_lettering_files(&self) -> Result<(), StoreError> {
        let dir = self.dir.join("candidates");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StoreError::io(&dir, error)),
        };
        let named: std::collections::HashSet<&str> = self.project.regions_untouched.iter()
            .filter_map(|row| Some(row.lettering.as_ref()?.mask_ref.as_str()))
            .collect();
        for entry in entries {
            let entry = entry.map_err(|error| StoreError::io(&dir, error))?;
            let path = entry.path();
            if !entry.file_type().map_err(|error| StoreError::io(&path, error))?.is_file() { continue; }
            if path.extension().and_then(|ext| ext.to_str()) != Some("mask") { continue; }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue };
            if !named.contains(format!("candidates/{name}").as_str()) {
                std::fs::remove_file(&path).map_err(|error| StoreError::io(&path, error))?;
            }
        }
        Ok(())
    }

    /// Delete text-group evidence files (`evidence/*.mtev`) no row names: a
    /// detection's group record, or the group snapshot of a patch row or a
    /// kept patch revision. One file is often named by many rows (a page's
    /// groups share it), so it goes only once none does. Run at open, under
    /// the job's lock, like the detection sweep.
    fn sweep_evidence_files(&self) -> Result<(), StoreError> {
        let dir = self.dir.join("evidence");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StoreError::io(&dir, error)),
        };
        let patches = self.project.patches.iter()
            .chain(&self.project.text_shape_patch_revisions)
            .chain(&self.project.legacy_patch_revisions);
        let named: std::collections::HashSet<String> = self.project.detections.iter()
            .filter_map(|row| row.group.as_ref()?.evidence_ref.clone())
            .chain(patches.filter_map(|row| {
                row.provenance.params_snapshot["group"]["evidence"].as_str().map(str::to_owned)
            }))
            .collect();
        for entry in entries {
            let entry = entry.map_err(|error| StoreError::io(&dir, error))?;
            let path = entry.path();
            if !entry.file_type().map_err(|error| StoreError::io(&path, error))?.is_file() { continue; }
            if path.extension().and_then(|ext| ext.to_str()) != Some("mtev") { continue; }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue };
            if !named.contains(&format!("evidence/{name}")) {
                std::fs::remove_file(&path).map_err(|error| StoreError::io(&path, error))?;
            }
        }
        Ok(())
    }

    /// Take a page's detections off the manifest **without flushing**, and
    /// answer with them so the caller can delete their files once a flush no
    /// longer names them ([`Job::delete_detection_files`]). Detect's reset,
    /// which drops a page's other rows in the same write.
    pub fn take_page_detections(&mut self, source_idx: usize) -> Vec<DetectedRegion> {
        let (taken, kept) = std::mem::take(&mut self.project.detections)
            .into_iter()
            .partition(|row| row.source_idx == source_idx);
        self.project.detections = kept;
        taken
    }

    /// Delete detections' sidecar files. Called only after the manifest that
    /// stopped naming them is on disk, so a crash between the two leaves an
    /// orphaned file and never a record pointing at nothing.
    pub fn delete_detection_files(&self, records: &[DetectedRegion]) {
        if self.read_only {
            return;
        }
        for record in records {
            for file in record.files() {
                let _ = std::fs::remove_file(self.dir.join(file));
            }
        }
    }

    /// Remove one detection, flush, and delete its files. It has no pixels to
    /// restore, so this is the whole of deleting one. `None` for an id no
    /// detection carries.
    pub fn remove_detection(&mut self, id: &str) -> Result<Option<DetectedRegion>, StoreError> {
        let _write = self.begin_write()?;
        let Some(position) = self.project.detections.iter().position(|row| row.id == id) else {
            return Ok(None);
        };
        let record = self.project.detections.remove(position);
        if let Err(error) = self.flush() {
            self.project.detections.insert(position, record);
            return Err(error);
        }
        self.delete_detection_files(std::slice::from_ref(&record));
        Ok(Some(record))
    }

    /// Replace a detection with the patch that cleaned it, **in one manifest
    /// write**: the record leaves in the same flush that names the patch, so no
    /// reader ever sees the region twice or not at all. The detection's files go
    /// after that flush. Every cleaner of a detection - a run, a region edit, a
    /// cloud render - commits through this.
    ///
    /// **Nothing is written outside the detection's display set**, its mask
    /// and its lettering together, which is the area the canvas draws and the
    /// selection tool edits. A model rung's isolation ring
    /// and a cloud render's grown hole both reach past it, so the patch's
    /// coverage (and its lettering) is cut back to that set here, where every
    /// cleaner of a detection meets, and its mask digest follows the cut.
    ///
    /// `Ok(false)` when no detection carries `id`, and nothing is written.
    /// The patch keeps its own id; every caller in this build passes the
    /// detection's.
    pub fn complete_detection(
        &mut self,
        id: &str,
        patch: &Patch,
        review_state: Option<String>,
    ) -> Result<bool, StoreError> {
        let _write = self.begin_write()?;
        let Some(found) = self.load_detection(id)? else {
            return Ok(false);
        };
        let clipped = clipped_to_display(patch, &found.mask, &found.ink);
        let patch = clipped.as_ref().unwrap_or(patch);
        let Some(position) = self.project.detections.iter().position(|row| row.id == id) else {
            return Ok(false);
        };
        let record = self.project.detections.remove(position);
        if let Err(error) = self.complete_display_region(record.source_idx, patch, review_state) {
            self.project.detections.insert(position, record);
            return Err(error);
        }
        self.delete_detection_files(std::slice::from_ref(&record));
        Ok(true)
    }

    /// Replace a detection with the untouched row a clean left it as, in one
    /// write, for the reason [`Job::complete_detection`] gives. The counters
    /// are the caller's, as they are for [`Job::leave_untouched`].
    ///
    /// The row keeps what a clean the user asks for needs: the box, and the
    /// lettering at the scale it was fitted at, stored as a held candidate's
    /// is, so that clean writes through the glyphs and not the box. A decline
    /// is the metric's verdict on one automatic pick, not on the region, and
    /// the region stays cleanable with a model the user names. Not
    /// `inside_bubble`, which marks a held candidate for the counters. A
    /// detection whose files cannot be read keeps its box alone, as rows did
    /// before.
    pub fn decline_detection(&mut self, id: &str, reason: &str) -> Result<bool, StoreError> {
        let _write = self.begin_write()?;
        let _detection_io = detection_io().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(position) = self.project.detections.iter().position(|row| row.id == id) else {
            return Ok(false);
        };
        let lettering = self.load_detection(id).ok().flatten()
            .map(|loaded| (loaded.ink, loaded.record.fit.map_or(1.0, |fit| fit.scale)));
        let record = self.project.detections.remove(position);
        let row = PendingUntouched {
            row: RegionUntouched {
                source_idx: record.source_idx,
                bbox: self.project.sources.get(record.source_idx).map_or(record.bbox, |source| source.orientation.rect(record.bbox,source.w,source.h)),
                reason: reason.to_owned(),
                inside_bubble: None,
                lettering: None,
            },
            lettering,
        };
        let rows = match self.write_held_lettering(vec![row]) {
            Ok(rows) => rows,
            Err(error) => {
                self.project.detections.insert(position, record);
                return Err(error);
            }
        };
        self.project.regions_untouched.extend(rows);
        if let Err(error) = self.flush() {
            self.project.regions_untouched.pop();
            self.project.detections.insert(position, record);
            return Err(error);
        }
        self.delete_detection_files(std::slice::from_ref(&record));
        Ok(true)
    }

    /// Record that the detector has looked at this source, and flush.
    ///
    /// Idempotent, because a page may be run more than once - a re-run over a
    /// chapter re-examines a page whose regions were all held back - and a
    /// list that grew on every pass would make `examined.len()` meaningless.
    ///
    /// Called *after* the page's regions are recorded, for the reason §6 gives
    /// for buffers before manifest: a crash between the two re-examines one
    /// page, which is work repeated, rather than skipping it, which is work
    /// lost.
    pub fn mark_examined(&mut self, source_idx: usize) -> Result<(), StoreError> {
        if self.project.examined.contains(&source_idx) {
            return Ok(());
        }
        self.project.examined.push(source_idx);
        self.flush()
    }

    /// Record that a run failed on this source, and flush.
    ///
    /// Idempotent for the reason [`mark_examined`] is, and with a sharper edge:
    /// the page is deliberately left unexamined so a later run tries it again,
    /// so without this the counter would be incremented once per attempt rather
    /// than once per failing page. `counters.errored` moves with the list and
    /// only with it.
    ///
    /// [`mark_examined`]: Job::mark_examined
    pub fn mark_errored(&mut self, source_idx: usize) -> Result<(), StoreError> {
        if self.project.errored.contains(&source_idx) {
            return Ok(());
        }
        self.project.errored.push(source_idx);
        self.project.counters.errored += 1;
        self.flush()
    }

    /// Take back a page's errored state, for a run that is about to re-attempt
    /// it. The counter follows the list down as well as up.
    ///
    /// Does not flush: the caller is `reset_page`, which drops a page's other
    /// records in the same breath and flushes once for all of them.
    pub fn clear_errored(&mut self, source_idx: usize) -> bool {
        if !self.project.errored.contains(&source_idx) {
            return false;
        }
        self.project.errored.retain(|index| *index != source_idx);
        self.project.counters.errored = self.project.counters.errored.saturating_sub(1);
        true
    }

    /// Record that this job stopped part-way, or that it did not - and flush.
    ///
    /// `page_index` is a position in `strip.order`, which is what
    /// `nextPageIndex` carries and
    /// what a resume picks up from. Passing `None` is how a completed run
    /// clears the offer, and it has to be the same call: a job that records
    /// being interrupted and has no way to say it no longer is would offer a
    /// resume for ever.
    pub fn mark_interrupted(&mut self, page_index: Option<u32>) -> Result<(), StoreError> {
        self.project.interrupted_at = page_index;
        self.flush()
    }

    /// Read one recorded patch back.
    ///
    /// A patch written before the ink file existed has none, and its absence
    /// is answered with the applied mask rather than an error: that is what
    /// such a patch's mask file used to say, and a re-run writes the narrower
    /// one. Any other failure to read it is the error it is.
    pub fn load_patch(&self, record: &PatchRecord) -> Result<Patch, StoreError> {
        let mask_path = self.dir.join(&record.mask_ref);
        let ink_path = self.dir.join(record.ink_ref());
        let buffer_path = self.dir.join(&record.buffer_ref);
        let mask = buffers::decode_mask(
            &std::fs::read(&mask_path).map_err(|e| StoreError::io(&mask_path, e))?,
        )?;
        if record.geometry_policy == GeometryPolicy::TextShape {
            let identity = record.text_shape_plan_identity.as_deref().ok_or(StoreError::StalePlan)?;
            let (_, prepared) = self
                .load_text_shape_plan_identity(&record.id, identity)?
                .ok_or(StoreError::StalePlan)?;
            if mask.bounds != prepared.write_support.bounds || mask.bits != prepared.write_support.bits {
                return Err(StoreError::StalePlan);
            }
        }
        let ink = match std::fs::read(&ink_path) {
            Ok(bytes) => buffers::decode_mask(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => mask.clone(),
            Err(e) => return Err(StoreError::io(&ink_path, e)),
        };
        let pixels = buffers::decode_patch(
            &std::fs::read(&buffer_path).map_err(|e| StoreError::io(&buffer_path, e))?,
        )?;
        Ok(Patch {
            id: record.id.clone(),
            mask,
            ink,
            pixels,
            order: record.order,
            visible: record.visible,
            provenance: record.provenance.clone(),
        }.presented())
    }

    /// Re-verify every source. §6, and the reason it is a hash and not an
    /// mtime: an mtime moves when nothing changed - a copy, a sync, a restore
    /// from backup - and stays put when a file is rewritten in place with the
    /// timestamp preserved. It is recorded because it is cheap and sometimes
    /// explanatory; it never decides.
    ///
    /// **What it can catch changed when ingest began importing pages.** It
    /// hashes the job's own copy, so it no longer notices a user re-cropping
    /// the scan they ingested from - that scan is not this chapter's page any
    /// more, and a re-crop is a new chapter. What it still catches is the
    /// failure §6 wrote it for: a
    /// page that is truncated, corrupted or gone underneath a job that is about
    /// to resume onto it, which is now the library's own file and so squarely
    /// this application's problem to report.
    pub fn verify_sources(&self) -> Vec<SourceState> {
        self.project
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let Some(path) = self.source_path(index) else {
                    return SourceState::Missing;
                };
                match std::fs::read(&path) {
                    Err(_) => SourceState::Missing,
                    Ok(bytes) => {
                        let sha256 = sha256_hex(&bytes);
                        if sha256 == source.sha256 {
                            SourceState::Unchanged
                        } else {
                            SourceState::Changed { sha256 }
                        }
                    }
                }
            })
            .collect()
    }

    /// Drop the patches of every stale source, and flush.
    ///
    /// §6: "On mismatch the page is marked stale, its patches are dropped, and
    /// the user is told." The last clause is why this returns a report instead
    /// of a count - a page that silently lost its edits is the failure the rule
    /// exists to prevent, and a caller cannot name the pages it was not given.
    pub fn drop_stale(&mut self, states: &[SourceState]) -> Result<StaleReport, StoreError> {
        let mut report = StaleReport::default();
        for (index, state) in states.iter().enumerate() {
            if state.is_stale() {
                report.sources.push(index);
            }
        }
        if report.is_empty() {
            return Ok(report);
        }
        let _write = self.begin_write()?;

        let stale = &report.sources;
        for record in &self.project.patches {
            if stale.contains(&record.source_idx) {
                report.dropped_patches.push(record.id.clone());
            }
        }
        self.project
            .patches
            .retain(|record| !stale.contains(&record.source_idx));
        self.project
            .text_shape_plans
            .retain(|record| !stale.contains(&record.source_idx));
        self.project
            .text_shape_patch_revisions
            .retain(|record| !stale.contains(&record.source_idx));
        self.project
            .legacy_patch_revisions
            .retain(|record| !stale.contains(&record.source_idx));
        self.project
            .text_shape_corrections
            .retain(|record| !stale.contains(&record.source_idx));
        self.project
            .regions_untouched
            .retain(|region| !stale.contains(&region.source_idx));
        // A detection's mask is coordinates on a page that is not there any
        // more, for the same reason its patches are.
        let mut detections = Vec::new();
        for source_idx in stale {
            detections.extend(self.take_page_detections(*source_idx));
        }
        // A source that changed has not been examined in its present form. The
        // patches go, and the record of having looked has to go with them, or a
        // re-crop would leave the page permanently out of the queue with
        // nothing on it.
        self.project.examined.retain(|index| !stale.contains(index));
        self.project.detected.retain(|index| !stale.contains(index));
        // And the record of having failed on it, with the counter it keeps in
        // step: a re-cropped page has not failed in its present form either.
        let before = self.project.errored.len();
        self.project.errored.retain(|index| !stale.contains(index));
        let dropped = (before - self.project.errored.len()) as u32;
        self.project.counters.errored = self.project.counters.errored.saturating_sub(dropped);
        self.flush()?;
        self.delete_detection_files(&detections);
        Ok(report)
    }

    /// Keep the files one page denoise run wrote, beside the runs before it.
    /// `rows` are one run and share a `created`. Flushes.
    ///
    /// A held row naming the same file as a new one goes: a rerun into the
    /// same folder wrote over that file, and the old row would describe
    /// pixels that are not there. A run recorded in the same second as a
    /// held one is moved to the second after the newest held run, so each
    /// `created` still names one run. Past [`MAX_DENOISE_RUNS`] runs the
    /// oldest go, taken or not.
    pub fn record_denoised(&mut self, mut rows: Vec<DenoisedPage>) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let held = &mut self.project.denoised;
        held.retain(|row| !rows.iter().any(|new| new.path == row.path));
        if rows.iter().any(|new| held.iter().any(|row| row.created == new.created)) {
            let next = held.iter().map(|row| row.created).max().unwrap_or(0) + 1;
            rows.iter_mut().for_each(|new| new.created = next);
        }
        held.extend(rows);
        let mut runs: Vec<u64> = held.iter().map(|row| row.created).collect();
        runs.sort_unstable_by(|a, b| b.cmp(a));
        runs.dedup();
        if let Some(&oldest_kept) = runs.get(MAX_DENOISE_RUNS - 1) {
            held.retain(|row| row.created >= oldest_kept);
        }
        held.sort_by_key(|row| (row.source_idx, row.created));
        self.flush()
    }

    /// Take `bytes`, a PNG of the page's displayed size, as source `source_idx`
    /// from now on: typically the file page denoise wrote for it. They are
    /// copied into the sidecar as `pages/denoised/<digest>/<page name>.png`,
    /// a directory of their own so the file keeps the page's name (an export
    /// names its files after it), and the source row names that file. The
    /// old file stays where it is.
    ///
    /// Only the source changes. Detections with their masks and lettering,
    /// and the examined and errored lists, stay: a page of the same size is
    /// the same page to every coordinate stored against it. The page's
    /// current denoise row ([`Project::current_denoised`]) stays as history,
    /// marked taken, and keeps the source it was made from, since the file
    /// it names is now the page and the raw page is only there.
    ///
    /// Refused, with nothing written, for a page with cleaning on it
    /// ([`Project::has_cleaning`]) or bytes that are not a whole PNG of the
    /// page's size. The manifest is not saved here: a batch of pages is one
    /// [`Job::flush`].
    pub fn replace_source(&mut self, source_idx: usize, bytes: &[u8]) -> Result<(), ReplaceError> {
        let _write = self.begin_write()?;
        let source = self.project.sources.get(source_idx).ok_or(ReplaceError::NoSource(source_idx))?;
        if self.project.has_cleaning(source_idx) {
            return Err(ReplaceError::Cleaned);
        }
        if crate::image::Format::sniff(bytes) != Some(crate::image::Format::Png) {
            return Err(ReplaceError::Unreadable("not a PNG".into()));
        }
        let page = crate::image::decode(bytes).map_err(|e| ReplaceError::Unreadable(e.to_string()))?;
        let (width,height)=source.orientation.size(source.w,source.h);
        if (page.width, page.height) != (width,height) {
            return Err(ReplaceError::SizeChanged { w: width, h: height, found_w: page.width, found_h: page.height });
        }
        // Denoise works in the visible page. Persist native samples and their
        // orientation so detections/history and standalone exports still agree.
        let owned_bytes = if source.orientation.0 == 1 { std::borrow::Cow::Borrowed(bytes) } else {
            let mut native=source.orientation.inverse().raster(&page);
            crate::image::exif::set_orientation(&mut native.color,source.orientation);
            std::borrow::Cow::Owned(crate::image::encode(&native,crate::image::Format::Png)
                .map_err(|e|ReplaceError::Unreadable(e.to_string()))?)
        };
        let sha256 = sha256_hex(&owned_bytes);
        let stem = source.rel_path.file_stem().map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("page-{source_idx}"));
        let dir = self.dir.join("pages").join("denoised").join(&sha256[..16]);
        std::fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;
        let path = dir.join(format!("{stem}.png"));
        buffers::write_atomic(&path, &owned_bytes)?;
        let rel_path = relative_to(self.root(), &path);
        let before = std::mem::replace(&mut self.project.sources[source_idx].rel_path, rel_path);
        if let Some(at) = newest_denoised(&self.project.denoised, source_idx) {
            self.project.denoised[at].taken = true;
        }
        // Every row of the page so far was made from the source it had until
        // now. An old row never said which, and after this its page's source
        // is no longer that file.
        for row in self.project.denoised.iter_mut().filter(|row| row.source_idx == source_idx) {
            row.source.get_or_insert_with(|| before.clone());
        }
        let row = &mut self.project.sources[source_idx];
        row.sha256 = sha256;
        row.mtime = mtime_of(&path);
        row.mode = page.mode;
        row.bit_depth = page.depth;
        Ok(())
    }

    /// Whether writing the chapter's output into `destination` would touch a
    /// source. §1's rule, checked before anything is written.
    pub fn output_refusal(&self, destination: &Path) -> Option<OutputRefusal> {
        let root = self.root();
        for (index, source) in self.project.sources.iter().enumerate() {
            // Both files, where a page has two. A converted page's input is
            // the JPEG the user still has, and an export that landed on top of
            // it would be output overwriting input however well the PNG beside
            // it was protected.
            let paths = [Some(&source.rel_path), source.converted_from.as_ref()];
            for path in paths.into_iter().flatten().map(|rel| root.join(rel)) {
                let Some(parent) = path.parent() else {
                    continue;
                };
                if same_directory(parent, destination) {
                    return Some(OutputRefusal::SourceDirectory { source_idx: index });
                }
                if let Some(name) = path.file_name() {
                    if same_directory(&destination.join(name), &path) {
                        return Some(OutputRefusal::WouldOverwriteSource { source_idx: index });
                    }
                }
            }
        }
        None
    }
}

/// §1's default: a sibling `<input>_cleaned/`.
pub fn default_output_dir(input_dir: &Path) -> PathBuf {
    let name = input_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    match (input_dir.parent(), name) {
        (Some(parent), Some(name)) if !name.is_empty() => parent.join(format!("{name}_cleaned")),
        // A path with no name to extend - a bare root, or the empty path. Put
        // the output under it rather than beside it, which is still not the
        // source directory.
        _ => input_dir.join("cleaned"),
    }
}

/// `<job>.mtclean` → `<job>.mtclean.d`.
///
/// Public because the sidecar is where everything a job owns that is not the
/// manifest lives - masks, patch pixels, and the undo journal - and a
/// caller that only wants the directory should not have to parse the
/// manifest to learn where it is. [`Job::sidecar`] is the same answer for a
/// job already open.
pub fn sidecar_dir(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".d");
    PathBuf::from(name)
}

/// Compare two paths without requiring either to exist.
///
/// `canonicalize` would be stronger - it resolves symlinks and `..` - but it
/// fails on a destination directory the user has just named and not yet
/// created, which is the common case for an export. So: canonical form where
/// both sides exist, lexical form where they do not.
fn same_directory(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => normalise(a) == normalise(b),
    }
}

fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `target` expressed relative to `base`, or `target` itself when no relative
/// form exists - a different volume, or a base that is not a prefix and cannot
/// be reached by `..` from a relative path.
fn relative_to(base: &Path, target: &Path) -> PathBuf {
    let (base, target) = (normalise(base), normalise(target));
    if base.is_absolute() != target.is_absolute() {
        return target;
    }

    let mut base_parts = base.components().peekable();
    let mut target_parts = target.components().peekable();
    while base_parts.peek().is_some() && base_parts.peek() == target_parts.peek() {
        base_parts.next();
        target_parts.next();
    }

    let mut out = PathBuf::new();
    for _ in base_parts {
        out.push("..");
    }
    for part in target_parts {
        out.push(part.as_os_str());
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// A path's last component, or the whole path where it has none.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn mtime_of(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod v3_manifest;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{decode, encode, fixtures, Format};
    use crate::mask::Mask;
    use crate::patch::Engine;

    /// A scratch directory that removes itself. `std::env::temp_dir` plus a
    /// counter rather than a crate: the tests need a unique path, not a
    /// dependency.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir()
                .join("cleaner-core-project")
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

    fn write_source(dir: &Path, name: &str, fixture: &str) -> SourceRef {
        std::fs::create_dir_all(dir).unwrap();
        let bytes = encode(&fixtures::by_name(fixture).raster, Format::Png).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        crate::ingest::source_ref(&path, &bytes).unwrap()
    }

    fn provenance() -> Provenance {
        Provenance {
            engine: Engine::Fill,
            engine_version: "test".into(),
            model_sha256: None,
            execution_provider: "cpu".into(),
            params_snapshot: serde_json::json!({ "thickness": 9 }),
            mask_sha256: "0".repeat(64),
            source_sha256: "1".repeat(64),
            cloud: None,
            created: 1_760_000_000,
        }
    }

    fn a_patch(id: &str) -> Patch {
        let bounds = Rect::new(8, 8, 12, 10);
        let page = fixtures::by_name("l8").raster;
        let mut pixels = page.clone();
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.data = vec![200; (bounds.w * bounds.h) as usize];
        Patch {
            id: id.into(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 3,
            visible: true,
            provenance: provenance(),
        }
    }

    fn a_job(scratch: &Scratch) -> Job {
        let source = write_source(&scratch.join("raws"), "001.png", "l8");
        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(
            manifest.parent().unwrap(),
            "0.1.0",
            StripMode::Single,
            &[source],
        );
        Job::create(&manifest, project).unwrap()
    }

    #[test]
    fn flushing_legacy_v3_does_not_add_empty_text_shape_plans() {
        let scratch = Scratch::new("v3-no-empty-plans");
        let job = a_job(&scratch);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        manifest.as_object_mut().unwrap().remove("text_shape_plans");
        std::fs::write(job.path(), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

        let reopened = Job::open(job.path()).unwrap();
        reopened.flush().unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        assert_eq!(saved["version"], 3);
        assert!(saved.get("text_shape_plans").is_none());
    }

    #[test]
    fn flushing_legacy_v3_with_patches_preserves_manifest_bytes() {
        let fixture = include_bytes!("fixtures/legacy_v3_patches.json");
        let old: v3_manifest::Project = serde_json::from_slice(fixture).unwrap();
        assert_eq!(old.patches.len(), 4);
        assert_eq!(serde_json::to_vec_pretty(&old).unwrap(), fixture);

        let scratch = Scratch::new("v3-patches-byte-for-byte");
        let path = scratch.join("chapter.mtclean");
        std::fs::write(&path, fixture).unwrap();
        let job = Job::open(&path).unwrap();
        assert_eq!(job.project.version, 3);
        job.flush().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), fixture);
    }

    #[test]
    fn legacy_edits_keep_v3_and_text_shape_plan_upgrades_to_v4() {
        let scratch = Scratch::new("v3-legacy-roundtrip");
        let mut job = a_job(&scratch);
        let path = job.path().to_path_buf();
        let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["version"] = serde_json::json!(3);
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        job = Job::open(&path).unwrap();
        job.complete_region(0, &a_patch("legacy"), None).unwrap();
        let old_revision = job.project.patches[0].legacy_revision_id().unwrap();
        let mut retry = a_patch("legacy");
        retry.pixels.data.fill(91);
        job.complete_region(0, &retry, None).unwrap();
        assert!(job.restore_legacy_revision("legacy", &old_revision).unwrap());
        let record = &mut job.project.patches[0];
        record.review_state = Some("review.reason.inputChanged".into());
        record.provenance.params_snapshot["input_provenance"] = serde_json::json!({
            "read_footprint": Rect::new(8, 8, 12, 10),
            "input_sha256": "earlier-composite"
        });
        record.provenance.params_snapshot["review_before_input_change"] = serde_json::Value::Null;
        job.flush().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let old: v3_manifest::Project = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(old.version, 3);
        assert_eq!(old.sources.len(), 1);
        assert_eq!(old.strip.order, vec![0]);
        assert_eq!(old.patches.len(), 1);
        let row = &old.patches[0];
        assert_eq!(row.id, "legacy");
        assert!(!row.mask_ref.is_empty() && !row.buffer_ref.is_empty());
        assert_eq!(row.engine, Engine::Fill);
        assert_eq!(row.order, 3);
        assert!(row.visible);
        assert_eq!(row.provenance.engine, Engine::Fill);
        assert_eq!(row.review_state.as_deref(), Some("review.reason.inputChanged"));
        assert_eq!(row.provenance.params_snapshot["input_provenance"]["input_sha256"], "earlier-composite");
        let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(saved["legacy_patch_revisions"].as_array().unwrap().len(), 2);
        assert!(saved["patches"][0].get("geometry_policy").is_none());

        let plan = job.plan_fixture("shape", 1, 0);
        let prepared = plan.prepare(job.project.sources[0].w, job.project.sources[0].h).unwrap();
        job.store_text_shape_plan(0, &plan, &prepared).unwrap();
        let upgraded: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(upgraded["version"], 4);
        job.project.text_shape_plans.clear();
        job.flush().unwrap();
        let rolled_back: serde_json::Value = serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        assert_eq!(rolled_back["version"], 3);
    }

    #[test]
    fn legacy_retry_restores_exact_pixels_after_reopen() {
        let scratch = Scratch::new("legacy-retry-revisions");
        let mut job = a_job(&scratch);
        let mut patch = a_patch("legacy");
        patch.pixels.data.fill(23);
        job.complete_region(0, &patch, None).unwrap();
        let first = job.project.patches[0].legacy_revision_id().unwrap();
        patch.pixels.data.fill(91);
        job.complete_region(0, &patch, None).unwrap();
        let second = job.project.patches[0].legacy_revision_id().unwrap();
        assert_ne!(first, second);
        let mut job = Job::open(job.path()).unwrap();
        assert!(job.restore_legacy_revision("legacy", &first).unwrap());
        assert!(job.load_patch(&job.project.patches[0]).unwrap().pixels.data.iter().all(|&p| p == 23));
        job = Job::open(job.path()).unwrap();
        assert!(job.restore_legacy_revision("legacy", &second).unwrap());
        assert!(job.load_patch(&job.project.patches[0]).unwrap().pixels.data.iter().all(|&p| p == 91));
    }

    #[test]
    fn old_legacy_sidecars_are_archived_before_retry() {
        let scratch = Scratch::new("legacy-pre-archive-retry");
        let mut job = a_job(&scratch);
        let mut patch = a_patch("legacy");
        patch.pixels.data.fill(7);
        let old = PatchRecord::of(0, &patch, None);
        buffers::write_atomic(&job.sidecar().join(&old.mask_ref), &buffers::encode_mask(&patch.mask)).unwrap();
        buffers::write_atomic(&job.sidecar().join(old.ink_ref()), &buffers::encode_mask(&patch.ink)).unwrap();
        buffers::write_atomic(&job.sidecar().join(&old.buffer_ref), &buffers::encode_patch(&patch.pixels)).unwrap();
        job.project.patches.push(old);
        job.flush().unwrap();

        patch.pixels.data.fill(99);
        job.complete_region(0, &patch, None).unwrap();
        let old_revision = job.project.legacy_patch_revisions.iter()
            .find(|row| row.buffer_ref != job.project.patches[0].buffer_ref)
            .unwrap().legacy_revision_id().unwrap();
        let new_revision = job.project.patches[0].legacy_revision_id().unwrap();
        let mut reopened = Job::open(job.path()).unwrap();
        assert!(reopened.restore_legacy_revision("legacy", &old_revision).unwrap());
        assert!(reopened.load_patch(&reopened.project.patches[0]).unwrap().pixels.data.iter().all(|&p| p == 7));
        assert!(reopened.restore_legacy_revision("legacy", &new_revision).unwrap());
        assert!(reopened.load_patch(&reopened.project.patches[0]).unwrap().pixels.data.iter().all(|&p| p == 99));
    }

    #[test]
    fn tampered_text_shape_mask_is_rejected_on_patch_load() {
        let scratch = Scratch::new("text-shape-tampered-mask");
        let mut job = a_job(&scratch);
        let plan = job.plan_fixture("shape", 1, 0);
        let prepared = plan.prepare(job.project.sources[0].w, job.project.sources[0].h).unwrap();
        job.store_text_shape_plan(0, &plan, &prepared).unwrap();
        let patch = Job::patch_with_support("shape", &prepared.write_support);
        job.complete_text_shape_region(0, &patch, &prepared.identity.identity_sha256, None).unwrap();
        let row = &job.project.patches[0];
        let mut mask = job.load_patch(row).unwrap().mask;
        let bounds = mask.bounds;
        let point = (bounds.y..bounds.bottom())
            .flat_map(|y| (bounds.x..bounds.right()).map(move |x| (x, y)))
            .find(|&(x, y)| !mask.contains(x, y))
            .unwrap();
        mask.set(point.0, point.1, true);
        std::fs::write(job.sidecar().join(&row.mask_ref), buffers::encode_mask(&mask)).unwrap();
        assert!(matches!(job.load_patch(row), Err(StoreError::StalePlan)));
    }

    #[test]
    fn a_manifest_round_trips_through_the_file_on_disk() {
        let scratch = Scratch::new("round-trip");
        let mut job = a_job(&scratch);
        job.project.settings = serde_json::json!({ "engineCeiling": "lama", "cloud": false });
        job.project.counters.declined = 2;
        job.complete_region(0, &a_patch("r0"), Some("review.reason.declined".into()))
            .unwrap();
        job.leave_untouched(
            0,
            Rect::new(4, 4, 6, 6),
            "review.reason.gateSkippedLowConfidence",
        )
        .unwrap();

        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(reopened.project, job.project);
        assert_eq!(reopened.project.patches[0].engine, Engine::Fill);
        assert_eq!(
            reopened.project.patches[0].provenance.params_snapshot,
            serde_json::json!({ "thickness": 9 }),
            "params_snapshot came back double-encoded"
        );
    }

    #[test]
    fn text_shape_plan_and_exact_support_survive_reopen_and_patch_commit() {
        let scratch = Scratch::new("text-shape-plan-roundtrip");
        let mut job = a_job(&scratch);
        let plan = job.plan_fixture("region-text", 1, 2);
        let prepared = plan
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        let support_hash = prepared.identity.support_sha256.clone();
        let identity_hash = prepared.identity.identity_sha256.clone();
        job.store_text_shape_plan(0, &plan, &prepared).unwrap();

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        assert_eq!(manifest["version"], 4);
        assert_eq!(manifest["text_shape_plans"].as_array().unwrap().len(), 1);
        assert!(
            manifest.to_string().len() < 30_000,
            "mask pixels must be stored in sidecar artifacts"
        );

        let mut reopened = Job::open(job.path()).unwrap();
        let (loaded_plan, loaded_prepared) = reopened
            .load_text_shape_plan("region-text")
            .unwrap()
            .expect("preview plan was persisted before apply");
        assert_eq!(loaded_plan, plan);
        assert_eq!(loaded_prepared, prepared);
        let validated = reopened
            .validate_text_shape_plan(
                "region-text",
                &plan.source_sha256,
                &plan.lower_composite_sha256,
                &identity_hash,
                &support_hash,
            )
            .unwrap();
        assert_eq!(validated.write_support, prepared.write_support);

        let patch = Job::patch_with_support("region-text", &prepared.write_support);
        reopened
            .complete_text_shape_region(0, &patch, &identity_hash, None)
            .unwrap();
        let row = &reopened.project.patches[0];
        assert_eq!(row.geometry_policy, GeometryPolicy::TextShape);
        assert_eq!(
            row.provenance.params_snapshot["geometry_policy"],
            "text_shape"
        );
        assert_eq!(
            row.provenance.params_snapshot["write_support_sha256"],
            support_hash
        );
        assert_eq!(
            row.text_shape_plan_identity.as_deref(),
            Some(identity_hash.as_str())
        );
        let saved_patch = reopened.load_patch(row).unwrap();
        assert_eq!(saved_patch.mask, prepared.write_support.to_mask());
    }

    #[test]
    fn text_shape_revisions_are_retained_and_stale_preview_values_are_rejected() {
        let scratch = Scratch::new("text-shape-plan-revisions");
        let mut job = a_job(&scratch);
        let first = job.plan_fixture("stable-region", 1, 2);
        let first_prepared = first
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &first, &first_prepared)
            .unwrap();
        let first_patch = Job::patch_with_support("stable-region", &first_prepared.write_support);
        job.complete_text_shape_region(
            0,
            &first_patch,
            &first_prepared.identity.identity_sha256,
            None,
        )
        .unwrap();
        let second = job.plan_fixture("stable-region", 2, 5);
        let second_prepared = second
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &second, &second_prepared)
            .unwrap();

        let reopened = Job::open(job.path()).unwrap();
        let latest = reopened
            .load_text_shape_plan("stable-region")
            .unwrap()
            .unwrap();
        assert_eq!(latest.0.plan_revision, 2);
        let old = reopened
            .load_text_shape_plan_identity(
                "stable-region",
                &first_prepared.identity.identity_sha256,
            )
            .unwrap()
            .unwrap();
        assert_eq!(old.1.write_support, first_prepared.write_support);
        assert_eq!(
            reopened.project.patches[0]
                .text_shape_plan_identity
                .as_deref(),
            Some(first_prepared.identity.identity_sha256.as_str()),
            "an applied patch keeps its plan identity across a newer preview"
        );
        assert!(matches!(
            reopened.validate_text_shape_plan(
                "stable-region",
                &first.source_sha256,
                "changed-lower-layer",
                &first_prepared.identity.identity_sha256,
                &first_prepared.identity.support_sha256,
            ),
            Err(StoreError::StalePlan)
        ));
        assert!(matches!(
            reopened.validate_text_shape_plan(
                "stable-region",
                &first.source_sha256,
                &first.lower_composite_sha256,
                &first_prepared.identity.identity_sha256,
                "bad-support-hash",
            ),
            Err(StoreError::StalePlan)
        ));
    }

    #[test]
    fn text_shape_commit_refuses_a_patch_mask_different_from_preview_support() {
        let scratch = Scratch::new("text-shape-plan-mismatch");
        let mut job = a_job(&scratch);
        let plan = job.plan_fixture("region-text", 1, 0);
        let prepared = plan
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &plan, &prepared).unwrap();
        let mut patch = Job::patch_with_support("region-text", &prepared.write_support);
        patch.mask.set(
            patch.mask.bounds.x,
            patch.mask.bounds.y,
            !patch
                .mask
                .contains(patch.mask.bounds.x, patch.mask.bounds.y),
        );
        assert!(matches!(
            job.complete_text_shape_region(0, &patch, &prepared.identity.identity_sha256, None),
            Err(StoreError::StalePlan)
        ));
        assert!(job.project.patches.is_empty());
    }

    #[test]
    fn reusing_plan_identity_cannot_replace_immutable_base_or_corrections() {
        let scratch = Scratch::new("text-shape-plan-identity-collision");
        let mut job = a_job(&scratch);
        let plan = job.plan_fixture("region-text", 1, 2);
        let prepared = plan
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &plan, &prepared).unwrap();

        // Add a base pixel that the unchanged correction removes. The final W
        // and identity are the same, but the immutable inputs are not; a
        // revision-reuse bug must not overwrite the first plan's sidecars.
        let mut alias = plan.clone();
        alias.base_mask.bits[1] = 255;
        alias.removals = MaskRaster {
            bounds: alias.base_mask.bounds,
            bits: vec![0, 255, 0, 0, 0, 0],
        };
        let alias_prepared = alias
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        assert_eq!(alias_prepared.identity, prepared.identity);
        assert!(matches!(
            job.store_text_shape_plan(0, &alias, &alias_prepared),
            Err(StoreError::Malformed(_))
        ));
        let (saved, _) = job
            .load_text_shape_plan_identity("region-text", &prepared.identity.identity_sha256)
            .unwrap()
            .unwrap();
        assert_eq!(saved, plan);
    }

    #[test]
    fn mixed_legacy_and_text_shape_patches_reopen_and_export_the_same_composite() {
        let scratch = Scratch::new("mixed-geometry-export");
        let mut job = a_job(&scratch);
        job.complete_region(0, &a_patch("legacy"), None).unwrap();

        let plan = job.plan_fixture("text-shape", 1, 2);
        let prepared = plan
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &plan, &prepared).unwrap();
        let mut text_patch = Job::patch_with_support("text-shape", &prepared.write_support);
        text_patch.order = 9;
        job.complete_text_shape_region(0, &text_patch, &prepared.identity.identity_sha256, None)
            .unwrap();

        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(reopened.project.patches.len(), 2);
        assert_eq!(
            reopened.project.patches[0].geometry_policy,
            GeometryPolicy::Legacy
        );
        assert_eq!(
            reopened.project.patches[1].geometry_policy,
            GeometryPolicy::TextShape
        );
        let patches: Vec<Patch> = reopened
            .project
            .patches
            .iter()
            .map(|record| reopened.load_patch(record).unwrap())
            .collect();
        let source_bytes = std::fs::read(reopened.source_path(0).unwrap()).unwrap();
        let source_page = decode(&source_bytes).unwrap();
        let preview = crate::composite::composite(&source_page, &patches).unwrap();
        let exported = crate::export::export_page(
            &source_bytes,
            &patches,
            crate::export::Target::SameAsSource,
        )
        .unwrap();
        let flattened = decode(&exported.bytes).unwrap();
        assert_eq!(flattened.data, preview.data);
        assert_eq!(flattened.mode, preview.mode);
        assert_eq!(flattened.depth, preview.depth);
    }

    #[test]
    fn text_shape_saved_patches_preserve_source_samples_mode_depth_and_icc_on_lossless_export() {
        for fixture in fixtures::all() {
            let scratch = Scratch::new(fixture.name);
            let format = if fixture.name == "cmyk8" {
                Format::Tiff
            } else {
                Format::Png
            };
            let source_bytes = encode(&fixture.raster, format).unwrap();
            let source_path = scratch.join(if format == Format::Tiff {
                "source.tiff"
            } else {
                "source.png"
            });
            std::fs::write(&source_path, &source_bytes).unwrap();
            let source = crate::ingest::source_ref(&source_path, &source_bytes).unwrap();
            let manifest = scratch.join("chapter.mtclean");
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
            let plan = job.plan_fixture("shape", 1, 2);
            let prepared = plan
                .prepare(fixture.raster.width, fixture.raster.height)
                .unwrap();
            job.store_text_shape_plan(0, &plan, &prepared).unwrap();
            let mut patch = Job::patch_with_support("shape", &prepared.write_support);
            let decoded_source = decode(&source_bytes).unwrap();
            patch.pixels = crate::engines::model::page_crop(&decoded_source, patch.mask.bounds);
            let (x, y) = (plan.base_mask.bounds.x, plan.base_mask.bounds.y);
            let (local_x, local_y) = (
                (x - patch.mask.bounds.x) as u32,
                (y - patch.mask.bounds.y) as u32,
            );
            let old = patch.pixels.sample(local_x, local_y, 0);
            patch.pixels.set_sample(local_x, local_y, 0, old ^ 1);
            assert!(patch.mask.contains(x, y));
            job.complete_text_shape_region(0, &patch, &prepared.identity.identity_sha256, None)
                .unwrap();

            let reopened = Job::open(&manifest).unwrap();
            let loaded = reopened.load_patch(&reopened.project.patches[0]).unwrap();
            let preview = crate::composite::composite(&decoded_source, std::slice::from_ref(&loaded)).unwrap();
            let output = crate::export::export_page(
                &source_bytes,
                &[loaded],
                crate::export::Target::SameAsSource,
            )
            .unwrap();
            let after = decode(&output.bytes).unwrap();
            assert_eq!(
                (after.mode, after.depth, &after.icc),
                (
                    decoded_source.mode,
                    decoded_source.depth,
                    &decoded_source.icc
                ),
                "{}",
                fixture.name
            );
            assert_eq!(after.data, preview.data, "{}", fixture.name);
            for y in 0..after.height {
                for x in 0..after.width {
                    if prepared.write_support.contains(x as i64, y as i64) {
                        continue;
                    }
                    for channel in 0..after.mode.samples() {
                        assert_eq!(
                            after.sample(x, y, channel),
                            decoded_source.sample(x, y, channel),
                            "{} ({x}, {y}) channel {channel}",
                            fixture.name
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn text_shape_visibility_undo_redo_keeps_the_prepared_support_identity() {
        let scratch = Scratch::new("text-shape-visibility-undo-redo");
        let mut job = a_job(&scratch);
        let plan = job.plan_fixture("text-shape", 1, 2);
        let prepared = plan
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &plan, &prepared).unwrap();
        let patch = Job::patch_with_support("text-shape", &prepared.write_support);
        job.complete_text_shape_region(0, &patch, &prepared.identity.identity_sha256, None)
            .unwrap();
        let identity = prepared.identity.identity_sha256.clone();

        for visible in [false, true, false, true] {
            job.project.patches[0].visible = visible;
            job.flush().unwrap();
            job = Job::open(job.path()).unwrap();
            let row = &job.project.patches[0];
            assert_eq!(row.visible, visible);
            assert_eq!(row.geometry_policy, GeometryPolicy::TextShape);
            assert_eq!(
                row.text_shape_plan_identity.as_deref(),
                Some(identity.as_str())
            );
            assert_eq!(
                job.load_patch(row).unwrap().mask.bits,
                prepared.write_support.bits
            );
            assert!(job
                .load_text_shape_plan_identity("text-shape", &identity)
                .unwrap()
                .is_some());
        }
    }

    #[test]
    fn successive_text_shape_revisions_restore_exact_patch_bytes_in_both_directions() {
        let scratch = Scratch::new("text-shape-successive-revision-undo");
        let mut job = a_job(&scratch);
        let first = job.plan_fixture("shape", 1, 2);
        let first_plan = first
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &first, &first_plan).unwrap();
        let mut first_patch = Job::patch_with_support("shape", &first_plan.write_support);
        first_patch.pixels.data.fill(21);
        job.complete_text_shape_region(0, &first_patch, &first_plan.identity.identity_sha256, None)
            .unwrap();
        let first_record = job.project.patches[0].clone();
        let first_revision = first_record.text_shape_revision_id().unwrap();

        let second = job.plan_fixture("shape", 2, 5);
        let second_plan = second
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &second, &second_plan).unwrap();
        let mut second_patch = Job::patch_with_support("shape", &second_plan.write_support);
        second_patch.pixels.data.fill(89);
        job.complete_text_shape_region(
            0,
            &second_patch,
            &second_plan.identity.identity_sha256,
            None,
        )
        .unwrap();
        let second_record = job.project.patches[0].clone();
        let second_revision = second_record.text_shape_revision_id().unwrap();
        assert_ne!(first_record.buffer_ref, second_record.buffer_ref);

        // A retry can keep the same plan identity while producing different
        // pixels. Its undo token must still name a distinct patch revision.
        let mut retry_patch = second_patch.clone();
        retry_patch.pixels.data.fill(117);
        job.complete_text_shape_region(
            0,
            &retry_patch,
            &second_plan.identity.identity_sha256,
            None,
        )
        .unwrap();
        let retry_record = job.project.patches[0].clone();
        let retry_revision = retry_record.text_shape_revision_id().unwrap();
        assert_ne!(second_revision, retry_revision);

        let mut reopened = Job::open(job.path()).unwrap();
        assert!(reopened
            .restore_text_shape_revision("shape", &first_revision)
            .unwrap());
        assert_eq!(reopened.project.patches[0], first_record);
        assert_eq!(
            reopened
                .load_patch(&reopened.project.patches[0])
                .unwrap()
                .pixels
                .data,
            first_patch.pixels.data
        );
        reopened = Job::open(job.path()).unwrap();
        assert!(reopened
            .restore_text_shape_revision("shape", &second_revision)
            .unwrap());
        assert_eq!(reopened.project.patches[0], second_record);
        assert_eq!(
            reopened
                .load_patch(&reopened.project.patches[0])
                .unwrap()
                .pixels
                .data,
            second_patch.pixels.data
        );
        reopened = Job::open(job.path()).unwrap();
        assert!(reopened
            .restore_text_shape_revision("shape", &retry_revision)
            .unwrap());
        assert_eq!(reopened.project.patches[0], retry_record);
        assert_eq!(
            reopened
                .load_patch(&reopened.project.patches[0])
                .unwrap()
                .pixels
                .data,
            retry_patch.pixels.data
        );
    }

    #[test]
    fn pre_archive_text_shape_patch_can_be_restored_after_a_new_retry() {
        let scratch = Scratch::new("text-shape-pre-archive-patch");
        let mut job = a_job(&scratch);
        let plan = job.plan_fixture("shape", 1, 2);
        let prepared = plan
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &plan, &prepared).unwrap();
        let mut patch = Job::patch_with_support("shape", &prepared.write_support);
        patch.pixels.data.fill(31);
        job.complete_text_shape_region(0, &patch, &prepared.identity.identity_sha256, None)
            .unwrap();
        // Simulate the uncommitted M3 sidecar naming, which used stable IDs.
        let row = &mut job.project.patches[0];
        for (from, to) in [
            (row.mask_ref.clone(), "shape.mask"),
            (row.ink_ref(), "shape.ink"),
            (row.buffer_ref.clone(), "shape.buf"),
        ] {
            std::fs::copy(job.dir.join(from), job.dir.join(to)).unwrap();
        }
        row.mask_ref = "shape.mask".into();
        row.buffer_ref = "shape.buf".into();
        let legacy_revision = row.text_shape_revision_id().unwrap();
        job.flush().unwrap();

        let mut retry = patch.clone();
        retry.pixels.data.fill(92);
        job.complete_text_shape_region(0, &retry, &prepared.identity.identity_sha256, None)
            .unwrap();
        let mut reopened = Job::open(job.path()).unwrap();
        assert!(reopened
            .restore_text_shape_revision("shape", &legacy_revision)
            .unwrap());
        assert_eq!(
            reopened
                .load_patch(&reopened.project.patches[0])
                .unwrap()
                .pixels
                .data,
            patch.pixels.data
        );
    }

    #[test]
    fn correction_state_survives_reopen_blocks_old_plan_and_clears_after_new_approval() {
        let scratch = Scratch::new("text-shape-correction-state");
        let mut job = a_job(&scratch);
        let first = job.plan_fixture("shape", 1, 2);
        let first_prepared = first
            .prepare(job.project.sources[0].w, job.project.sources[0].h)
            .unwrap();
        job.store_text_shape_plan(0, &first, &first_prepared)
            .unwrap();
        job.record_text_shape_correction(
            0,
            "shape",
            2,
            Rect::new(8, 8, 10, 10),
            "lettering mask is empty",
        )
        .unwrap();
        let mut reopened = Job::open(job.path()).unwrap();
        assert_eq!(
            reopened.text_shape_correction("shape").unwrap().reason,
            "lettering mask is empty"
        );
        let second = reopened.plan_fixture("shape", 2, 5);
        let second_prepared = second
            .prepare(reopened.project.sources[0].w, reopened.project.sources[0].h)
            .unwrap();
        assert!(matches!(
            reopened.store_text_shape_plan(0, &second, &second_prepared),
            Err(StoreError::StalePlan)
        ));
        let third = reopened.plan_fixture("shape", 3, 5);
        let third_prepared = third
            .prepare(reopened.project.sources[0].w, reopened.project.sources[0].h)
            .unwrap();
        reopened
            .store_text_shape_plan(0, &third, &third_prepared)
            .unwrap();
        let reopened = Job::open(job.path()).unwrap();
        assert!(reopened.text_shape_correction("shape").is_none());
        assert_eq!(
            reopened
                .load_text_shape_plan("shape")
                .unwrap()
                .unwrap()
                .0
                .plan_revision,
            3
        );
    }

    /// The manifest is a human-readable record of a job, and §1 fixes the field
    /// names. A rename here is a break for anything reading it.
    #[test]
    fn the_manifest_has_the_field_names_the_document_specifies() {
        let scratch = Scratch::new("field-names");
        let mut job = a_job(&scratch);
        job.complete_region(0, &a_patch("r0"), None).unwrap();
        job.leave_untouched(0, Rect::new(4, 4, 6, 6), "decline.reason.qualityMetric")
            .unwrap();

        let text = std::fs::read_to_string(job.path()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        for key in [
            "version",
            "created",
            "app_version",
            "sources",
            "strip",
            "patches",
            "regions_untouched",
            "counters",
            "settings",
        ] {
            assert!(value.get(key).is_some(), "the manifest has no {key}");
        }
        for key in ["rel_path", "sha256", "mtime", "w", "h", "mode", "bit_depth"] {
            assert!(
                value["sources"][0].get(key).is_some(),
                "a source row has no {key}"
            );
        }
        for key in [
            "id",
            "source_idx",
            "bbox",
            "mask_ref",
            "buffer_ref",
            "engine",
            "order",
            "visible",
            "review_state",
            "provenance",
        ] {
            assert!(
                value["patches"][0].get(key).is_some(),
                "a patch row has no {key}"
            );
        }
        for key in [
            "refused",
            "errored",
            "param_reject",
            "residual_reject",
            "structural_reject",
            "declined",
            "gate_dropped",
        ] {
            assert!(
                value["counters"].get(key).is_some(),
                "the counters have no {key}"
            );
        }
        // The vocabulary a reader expects: mode names and a bit depth that
        // is a number.
        assert_eq!(value["sources"][0]["mode"], "L");
        assert_eq!(value["sources"][0]["bit_depth"], 8);
        assert_eq!(value["patches"][0]["engine"], "fill");
    }

    #[test]
    fn a_completed_region_is_on_disk_before_the_call_returns() {
        let scratch = Scratch::new("flush");
        let mut job = a_job(&scratch);
        let patch = a_patch("r0");
        job.complete_region(0, &patch, None).unwrap();

        // Nothing else is called - no explicit save, no drop. §6's flush is
        // what makes a crash here survivable.
        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(reopened.project.patches.len(), 1);
        let loaded = reopened.load_patch(&reopened.project.patches[0]).unwrap();
        assert_eq!(loaded.mask, patch.mask);
        assert_eq!(loaded.ink, patch.ink);
        assert_eq!(loaded.pixels.data, patch.pixels.data);
    }

    /// A job written before the ink file existed has a `.mask` and a `.buf`
    /// per patch and nothing else. It still opens, and the ink it answers is
    /// the applied mask - what its mask file used to be made of.
    #[test]
    fn a_patch_without_an_ink_file_answers_its_applied_mask_as_ink() {
        let scratch = Scratch::new("no-ink-file");
        let mut job = a_job(&scratch);
        let mut patch = a_patch("r0");
        patch.ink = Mask::filled(Rect::new(10, 10, 4, 3));
        job.complete_region(0, &patch, None).unwrap();
        let record = job.project.patches[0].clone();
        assert!(
            job.sidecar().join(record.ink_ref()).exists(),
            "the ink is a sidecar file"
        );

        std::fs::remove_file(job.sidecar().join(record.ink_ref())).unwrap();
        let loaded = job.load_patch(&record).unwrap();
        assert_eq!(
            loaded.ink, patch.mask,
            "no ink file: the applied mask stands in"
        );
    }

    #[test]
    fn re_running_a_region_replaces_its_record_rather_than_doubling_it() {
        let scratch = Scratch::new("rerun");
        let mut job = a_job(&scratch);
        job.complete_region(0, &a_patch("r0"), None).unwrap();
        let mut second = a_patch("r0");
        second.provenance.engine = Engine::Lama;
        second.order = 7;
        job.complete_region(0, &second, None).unwrap();

        assert_eq!(job.project.patches.len(), 1);
        assert_eq!(job.project.patches[0].engine, Engine::Lama);
        assert_eq!(job.project.patches[0].order, 7);
    }

    #[test]
    fn a_saved_denoise_pick_reads_as_a_fill() {
        let scratch = Scratch::new("denoise-pick");
        let mut job = a_job(&scratch);
        let mut value = serde_json::to_value(a_detection(&mut job, Rect::new(4, 4, 8, 8))).unwrap();
        value["pick"] = serde_json::json!("denoise");
        let read: DetectedRegion = serde_json::from_value(value).unwrap();
        assert_eq!(read.pick, "fill");
    }

    fn a_detection(job: &mut Job, bbox: Rect) -> DetectedRegion {
        DetectedRegion {
            id: job.next_detection_id("c1-p001"),
            source_idx: 0,
            bbox,
            mask_ref: String::new(),
            inside: true,
            balloon_color: Some([250, 250, 250]),
            script: Some("ja".into()),
            pick: "fill".into(),
            detector: "local".into(),
            created: 1_760_000_000,
            mask_sha256: String::new(),
            order: 2,
            review_state: None,
            fit: Some(DetectionFit {
                route: crate::fit::Route::Fill,
                thickness: 5,
                best_deviation: 1.5,
                scale: 1.0,
            }),
            group: None,
            padding_from_seed: false,
            padding_px: 0,
        }
    }

    #[test]
    fn oriented_detection_padding_and_declined_lettering_stay_native_on_disk() {
        for value in 2..=8 {
            let scratch = Scratch::new(&format!("oriented-detection-{value}"));
            let mut job = a_job(&scratch);
            job.project.sources[0].orientation = crate::image::orientation::Orientation(value);
            let source = job.project.sources[0].clone();
            let base = Mask::filled(Rect::new(8,12,5,7));
            let mut grown = Mask::filled(Rect::new(6,10,9,11)); grown.bits[3]=37;
            let mut record = a_detection(&mut job, base.bounds); record.padding_px=2;
            job.replace_page_detection_results(0,"c1-p001",vec![LoadedDetection {
                record,mask:grown.clone(),ink:base.clone(),evidence:None,base:Some((base.clone(),base.clone()))
            }],Vec::<RegionUntouched>::new()).unwrap();
            let stored = job.project.detections[0].clone();
            let (w,h)=source.orientation.size(source.w,source.h);
            assert_eq!(stored.bbox,source.orientation.inverse().rect(base.bounds,w,h));
            let disk=buffers::decode_mask(&std::fs::read(job.sidecar().join(&stored.mask_ref)).unwrap()).unwrap();
            assert_eq!(disk,source.orientation.inverse().mask(&grown,w,h));
            let loaded=Job::open(job.path()).unwrap();
            assert_eq!(loaded.load_detection(&stored.id).unwrap().unwrap().mask,grown);
            assert_eq!(loaded.load_detection_base(&stored.id).unwrap().unwrap(),(base.clone(),base.clone()));
            job.decline_detection(&stored.id,"declined").unwrap();
            let row=&job.project.regions_untouched[0];
            assert_eq!(row.bbox,stored.bbox);
            assert_eq!(job.load_held_lettering(row).unwrap().unwrap().0,base);
        }
    }

    /// A padded detection keeps its ungrown masks beside the grown ones, and
    /// one written without them is refused: its padding could never be
    /// changed again.
    #[test]
    fn a_padded_detection_is_stored_with_its_unpadded_masks_or_not_at_all() {
        let scratch = Scratch::new("detection-padding");
        let mut job = a_job(&scratch);
        let (base, grown) = (Mask::filled(Rect::new(8, 8, 4, 4)), Mask::filled(Rect::new(6, 6, 8, 8)));
        let mut record = a_detection(&mut job, Rect::new(8, 8, 4, 4));
        record.padding_px = 2;
        assert!(matches!(job.store_detection(record.clone(), &grown, &grown), Err(StoreError::Malformed(_))));
        job.replace_page_detection_results(0, "c1-p001", vec![LoadedDetection {
            record, mask: grown.clone(), ink: grown.clone(), evidence: None, base: Some((base.clone(), base.clone())),
        }], Vec::<RegionUntouched>::new()).unwrap();

        let reopened = Job::open(job.path()).unwrap();
        let id = reopened.project.detections[0].id.clone();
        let stored = reopened.load_detection(&id).unwrap().unwrap();
        assert_eq!((stored.record.padding_px, &stored.mask), (2, &grown));
        assert_eq!(reopened.load_detection_base(&id).unwrap(), Some((base.clone(), base)));
        assert!(reopened.sidecar().join(stored.record.base_mask_ref()).exists());
    }

    #[test]
    fn a_detection_survives_reopen_and_its_patch_replaces_it_in_one_write() {
        let scratch = Scratch::new("detection-roundtrip");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let ink = Mask::filled(Rect::new(10, 10, 6, 4));
        let record = a_detection(&mut job, Rect::new(9, 9, 10, 8));
        assert_eq!(record.id, "c1-p001-d0");
        job.store_detection(record, &mask, &ink).unwrap();

        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(reopened.project.counters.detections, 1);
        let loaded = reopened.load_detection("c1-p001-d0").unwrap().expect("stored");
        assert_eq!(loaded.record.mask_ref, "detections/c1-p001-d0.mask");
        assert_eq!(loaded.record.mask_sha256, sha256_hex(&buffers::encode_mask(&mask)));
        assert_eq!((loaded.mask, loaded.ink), (mask, ink));
        assert_eq!(loaded.record.fit.unwrap().route, crate::fit::Route::Fill);

        let mut job = reopened;
        let mut patch = a_patch("c1-p001-d0");
        patch.order = loaded.record.order;
        assert!(job.complete_detection("c1-p001-d0", &patch, None).unwrap());
        let reopened = Job::open(job.path()).unwrap();
        assert!(reopened.project.detections.is_empty());
        assert_eq!(reopened.project.patches.len(), 1);
        assert_eq!(reopened.project.patches[0].id, "c1-p001-d0");
        assert!(!job.sidecar().join("detections/c1-p001-d0.mask").exists());
        assert!(!job.sidecar().join("detections/c1-p001-d0.ink").exists());
        // The patch's own compatibility ink sits at the root and is untouched.
        assert!(job.sidecar().join("c1-p001-d0.ink").exists());
        // The allocator never hands the id out again.
        assert_eq!(job.next_detection_id("c1-p001"), "c1-p001-d1");
    }

    #[test]
    fn a_detection_counts_its_hand_edits_from_its_file_name() {
        let scratch = Scratch::new("detection-edits");
        let mut job = a_job(&scratch);
        let mut record = a_detection(&mut job, Rect::new(0, 0, 4, 4));
        record.mask_ref = format!("detections/{}.mask", record.id);
        assert_eq!(record.edits(), 0);
        record.mask_ref = format!("detections/{}.e7.mask", record.id);
        assert_eq!(record.edits(), 7);
        record.mask_ref = "detections/other.e3.mask".into();
        assert_eq!(record.edits(), 0, "another id's file is not this one's edit");
    }

    /// A rung that writes past the drawn mask - a model's
    /// isolation ring, a cloud render's grown hole - is cut back to the
    /// detection's mask and lettering when its patch replaces it.
    #[test]
    fn a_patch_replacing_a_detection_writes_nothing_outside_its_display_set() {
        let scratch = Scratch::new("detection-clip");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(10, 10, 6, 5));
        // Lettering reaching one pixel past the mask's right edge.
        let ink = Mask::filled(Rect::new(14, 11, 3, 2));
        let record = a_detection(&mut job, Rect::new(10, 10, 6, 5));
        job.store_detection(record, &mask, &ink).unwrap();

        // Covers (8, 8) to (19, 17), wider than both.
        let patch = a_patch("c1-p001-d0");
        assert!(job.complete_detection("c1-p001-d0", &patch, None).unwrap());
        let reopened = Job::open(job.path()).unwrap();
        let stored = reopened.load_patch(&reopened.project.patches[0]).unwrap();
        assert_eq!(stored.mask.bounds, patch.mask.bounds, "bounds and pixels are kept");
        for y in stored.mask.bounds.y..stored.mask.bounds.bottom() {
            for x in stored.mask.bounds.x..stored.mask.bounds.right() {
                let shown = mask.contains(x, y) || ink.contains(x, y);
                assert_eq!(stored.mask.coverage(x, y), if shown { 255 } else { 0 }, "mask at ({x}, {y})");
                assert_eq!(stored.ink.coverage(x, y), if shown { 255 } else { 0 }, "ink at ({x}, {y})");
            }
        }
        assert_eq!(stored.provenance.mask_sha256, sha256_hex(&buffers::encode_mask(&stored.mask)));
    }

    #[test]
    fn damaged_detection_mask_or_missing_recorded_ink_is_refused() {
        let scratch = Scratch::new("detection-damage");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let record = a_detection(&mut job, mask.bounds);
        let id = record.id.clone();
        job.store_detection(record, &mask, &mask).unwrap();
        let ink = job.sidecar().join(format!("detections/{id}.ink"));
        std::fs::remove_file(&ink).unwrap();
        assert!(matches!(job.load_detection(&id), Err(StoreError::Io { .. })));
        std::fs::write(&ink, buffers::encode_mask(&mask)).unwrap();
        std::fs::write(job.sidecar().join(format!("detections/{id}.mask")), b"tampered").unwrap();
        assert!(matches!(job.load_detection(&id), Err(StoreError::Malformed(_))));
    }

    /// A denoised file taken as the page keeps every detection, and the
    /// reopened job verifies it: nothing is stale, so nothing is dropped.
    #[test]
    fn a_replaced_source_keeps_its_detections_and_verifies() {
        let scratch = Scratch::new("replace-source");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let record = a_detection(&mut job, mask.bounds);
        let id = record.id.clone();
        job.store_detection(record, &mask, &mask).unwrap();
        job.mark_examined(0).unwrap();
        job.record_denoised(vec![DenoisedPage {
            source_idx: 0, path: scratch.join("denoised/001.png"), appearance: "look".into(), created: 1,
            ..Default::default()
        }]).unwrap();
        let old_path = job.source_path(0).unwrap();
        let old_rel = job.project.sources[0].rel_path.clone();
        let old_sha = job.project.sources[0].sha256.clone();

        let mut page = fixtures::by_name("l8").raster;
        page.data.iter_mut().for_each(|value| *value = value.wrapping_add(7));
        let bytes = encode(&page, Format::Png).unwrap();
        job.replace_source(0, &bytes).unwrap();
        job.flush().unwrap();

        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(reopened.project.sources[0].sha256, sha256_hex(&bytes));
        assert_ne!(reopened.project.sources[0].sha256, old_sha);
        assert_eq!(std::fs::read(reopened.source_path(0).unwrap()).unwrap(), bytes);
        assert_eq!(reopened.source_path(0).unwrap().file_name().unwrap(), "001.png", "the page keeps its name");
        assert!(old_path.is_file(), "the old page stays on disk");
        assert!(reopened.verify_sources().iter().all(|state| !state.is_stale()));
        assert_eq!(reopened.load_detection(&id).unwrap().expect("kept").mask, mask);
        assert_eq!(reopened.project.examined, [0]);
        let [row] = reopened.project.denoised.as_slice() else { panic!("the denoise row stays as history") };
        assert!(row.taken);
        assert_eq!(row.source.as_ref(), Some(&old_rel), "it keeps the raw page it was made from");
        assert!(reopened.project.current_denoised(0).is_none(), "and is no longer the page's to take");
    }

    #[test]
    fn oriented_denoise_replacement_keeps_native_detections_originals_and_export_orientation() {
        for value in 1..=8 {
            let scratch=Scratch::new(&format!("replace-oriented-{value}"));let mut job=a_job(&scratch);
            let orientation=crate::image::orientation::Orientation(value);job.project.sources[0].orientation=orientation;
            let original_path=job.source_path(0).unwrap();let original_bytes=std::fs::read(&original_path).unwrap();
            let original=crate::image::decode(&original_bytes).unwrap();
            let native_mask=Mask::filled(Rect::new(8,8,12,10));let mask=orientation.mask(&native_mask,original.width,original.height);
            let record=a_detection(&mut job,mask.bounds);let id=record.id.clone();job.store_detection(record,&mask,&mask).unwrap();
            let stored=job.project.detections[0].clone();let stored_bytes=std::fs::read(job.sidecar().join(&stored.mask_ref)).unwrap();
            let history=job.sidecar().join("history.json");std::fs::write(&history,b"saved undo history").unwrap();
            let mut display=orientation.raster(&original);display.depth=crate::image::BitDepth::Sixteen;display.data=vec![0;display.stride()*display.height as usize];
            display.srgb_intent=None;display.color.gamma=Some(50_000);
            for y in 0..display.height {for x in 0..display.width {display.set_sample(x,y,0,((x*257+y*113+19)%65536) as u16);}}
            let bytes=encode(&display,Format::Png).unwrap();job.replace_source(0,&bytes).unwrap();job.flush().unwrap();
            let reopened=Job::open(job.path()).unwrap();let owned=std::fs::read(reopened.source_path(0).unwrap()).unwrap();let native=crate::image::decode(&owned).unwrap();
            assert_eq!((native.width,native.height),(original.width,original.height));assert_eq!(native.depth,display.depth);
            assert_eq!(native.color.gamma,display.color.gamma);assert_eq!(crate::image::orientation::from_bytes(&owned),orientation);
            assert_eq!(orientation.raster(&native).data,display.data);assert_eq!(reopened.project.sources[0].sha256,sha256_hex(&owned));
            assert_eq!(reopened.project.detections[0],stored);assert_eq!(std::fs::read(reopened.sidecar().join(&stored.mask_ref)).unwrap(),stored_bytes);
            assert_eq!(reopened.load_detection(&id).unwrap().unwrap().mask,mask);assert_eq!(std::fs::read(history).unwrap(),b"saved undo history");
            assert_eq!(std::fs::read(original_path).unwrap(),original_bytes);assert!(reopened.verify_sources().iter().all(|state|!state.is_stale()));
            let exported=crate::export::export_page(&owned,&[],crate::export::Target::Explicit(Format::Tiff)).unwrap();let exported=crate::image::decode(&exported.bytes).unwrap();
            assert_eq!((exported.width,exported.height),(display.width,display.height));assert_eq!(exported.data,display.data);
        }
    }

    fn a_denoised_row(source_idx: usize, path: &str, created: u64) -> DenoisedPage {
        DenoisedPage { source_idx, path: PathBuf::from(path), appearance: "look".into(), created, ..Default::default() }
    }

    /// A second run keeps the first beside it; the page's current file is the
    /// newest one; a rerun into the same folder replaces only the rows whose
    /// files it wrote over; and a run in the same second as a held one is
    /// moved on, so each `created` still names one run.
    #[test]
    fn denoise_runs_are_kept_as_history_and_the_newest_is_current() {
        let scratch = Scratch::new("denoise-history");
        let mut job = a_job(&scratch);
        job.record_denoised(vec![a_denoised_row(0, "/d/a/001.png", 10), a_denoised_row(1, "/d/a/002.png", 10)]).unwrap();
        let mut second = a_denoised_row(0, "/d/b/001.png", 20);
        second.preset = Some("waifu2x-scan-4x-n2".into());
        second.target = Some(crate::cloud_denoise_wire::PresetTarget::Local);
        job.record_denoised(vec![second]).unwrap();

        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(reopened.project.denoised.len(), 3, "both runs are kept");
        let current = reopened.project.current_denoised(0).unwrap();
        assert_eq!((current.created, current.preset.as_deref()), (20, Some("waifu2x-scan-4x-n2")));
        assert_eq!(reopened.project.current_denoised(1).unwrap().created, 10);
        assert!(reopened.project.current_denoised(2).is_none());

        job.record_denoised(vec![a_denoised_row(1, "/d/a/002.png", 30)]).unwrap();
        let rows: Vec<(usize, u64)> = job.project.denoised.iter().map(|row| (row.source_idx, row.created)).collect();
        assert_eq!(rows, [(0, 10), (0, 20), (1, 30)], "the overwritten file's old row goes");

        job.record_denoised(vec![a_denoised_row(0, "/d/c/001.png", 30)]).unwrap();
        assert_eq!(job.project.current_denoised(0).unwrap().created, 31, "a run in the same second is moved on");
    }

    #[test]
    fn denoise_history_keeps_the_newest_runs() {
        let scratch = Scratch::new("denoise-cap");
        let mut job = a_job(&scratch);
        for run in 1..=(MAX_DENOISE_RUNS as u64 + 3) {
            job.record_denoised(vec![a_denoised_row(0, &format!("/d/{run}/001.png"), run * 100)]).unwrap();
        }
        let runs: Vec<u64> = job.project.denoised.iter().map(|row| row.created).collect();
        assert_eq!(runs.len(), MAX_DENOISE_RUNS);
        assert_eq!(runs.first(), Some(&400), "the three oldest runs went");
        assert_eq!(job.project.current_denoised(0).unwrap().created, (MAX_DENOISE_RUNS as u64 + 3) * 100);
    }

    /// A row written before the history kept presets and targets still opens,
    /// with nothing to say about them, and flushes back without the new keys.
    #[test]
    fn an_old_denoise_row_opens_without_the_new_fields() {
        let row: DenoisedPage = serde_json::from_value(serde_json::json!({
            "source_idx": 3, "path": "/Users/x/denoised-cleaned/004.png", "appearance": "abc", "created": 1790000000,
        })).unwrap();
        assert_eq!(row, DenoisedPage {
            source_idx: 3, path: "/Users/x/denoised-cleaned/004.png".into(), appearance: "abc".into(),
            created: 1_790_000_000, ..Default::default()
        });
        let back = serde_json::to_value(&row).unwrap();
        assert_eq!(back.as_object().unwrap().len(), 4, "{back}");
    }

    #[test]
    fn a_source_is_kept_for_a_cleaned_page_or_a_wrong_file() {
        let scratch = Scratch::new("replace-refused");
        let mut job = a_job(&scratch);
        let before = job.project.sources[0].clone();
        let page = fixtures::by_name("l8").raster;
        let mut narrow = page.clone();
        narrow.width -= 1;
        narrow.data = vec![0; narrow.stride() * narrow.height as usize];
        assert!(matches!(job.replace_source(0, &encode(&narrow, Format::Png).unwrap()),
            Err(ReplaceError::SizeChanged { .. })));
        assert!(matches!(job.replace_source(0, b"not a page"), Err(ReplaceError::Unreadable(_))));
        assert!(matches!(job.replace_source(3, &encode(&page, Format::Png).unwrap()), Err(ReplaceError::NoSource(3))));

        assert!(!job.project.has_cleaning(0));
        let mut hidden = PatchRecord::of(0, &a_patch("c1-p001-d0"), None);
        hidden.visible = false;
        job.project.patches.push(hidden);
        assert!(job.project.has_cleaning(0), "a hidden patch is cleaning too");
        assert!(matches!(job.replace_source(0, &encode(&page, Format::Png).unwrap()), Err(ReplaceError::Cleaned)));
        assert_eq!(job.project.sources[0], before);
        assert!(!job.sidecar().join("pages").exists(), "nothing was written");
    }

    #[test]
    fn opening_sweeps_only_unreferenced_detection_sidecars() {
        let scratch = Scratch::new("detection-orphans");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let record = a_detection(&mut job, mask.bounds);
        let id = record.id.clone();
        job.store_detection(record, &mask, &mask).unwrap();
        let stray = job.sidecar().join("detections/orphan.mask");
        std::fs::write(&stray, b"orphan").unwrap();
        let unrelated = job.sidecar().join("keep.mask");
        std::fs::write(&unrelated, b"keep").unwrap();
        let reopened = Job::open(job.path()).unwrap();
        assert!(!stray.exists());
        assert!(unrelated.exists());
        assert!(reopened.load_detection(&id).unwrap().is_some());
    }

    /// A page's untouched rows are one manifest write, and none is no write.
    #[test]
    fn untouched_rows_are_written_together() {
        let scratch = Scratch::new("untouched-rows");
        let mut job = a_job(&scratch);
        let before = job.generation();
        job.leave_untouched_rows(Vec::<RegionUntouched>::new()).unwrap();
        assert_eq!(job.generation(), before, "no rows wrote the manifest");
        let row = |x| RegionUntouched { source_idx: 0, bbox: Rect::new(x, 2, 4, 4), reason: "review.reason.unassignedMask".into(), inside_bubble: None, lettering: None };
        job.leave_untouched_rows(vec![row(2), row(10), row(20)]).unwrap();
        assert_ne!(job.generation(), before);
        assert_eq!(Job::open(job.path()).unwrap().project.regions_untouched.len(), 3);
    }

    /// A held candidate keeps its lettering across a reopen, glyph for glyph,
    /// and the open's sweep takes only the lettering no row names.
    #[test]
    fn a_held_candidate_keeps_its_lettering_and_the_sweep_keeps_what_rows_name() {
        let scratch = Scratch::new("held-lettering");
        let mut job = a_job(&scratch);
        let mut lettering = Mask::empty(Rect::new(10, 10, 12, 6));
        for (x, y) in [(10, 10), (11, 10), (11, 11), (20, 14), (21, 15)] {
            lettering.set(x, y, true);
        }
        let held = |bbox, lettering| PendingUntouched {
            row: RegionUntouched { source_idx: 0, bbox, reason: "review.reason.unassignedMask".into(),
                inside_bubble: Some(false), lettering: None },
            lettering,
        };
        job.leave_untouched_rows(vec![held(lettering.bounds, Some((lettering.clone(), 1.5))),
            held(Rect::new(40, 40, 4, 4), None)]).unwrap();
        let stray = job.sidecar().join("candidates/orphan.mask");
        std::fs::write(&stray, b"orphan").unwrap();

        let reopened = Job::open(job.path()).unwrap();
        assert!(!stray.exists(), "a file no row names was kept");
        let rows = &reopened.project.regions_untouched;
        let held = rows[0].lettering.as_ref().expect("the lettering is named on its row");
        assert!(held.mask_ref.starts_with("candidates/") && held.mask_ref.ends_with(".mask"));
        assert!(reopened.sidecar().join(&held.mask_ref).exists());
        assert_eq!(reopened.load_held_lettering(&rows[0]).unwrap(), Some((lettering, 1.5)));
        assert_eq!(rows[1].lettering, None);
        assert_eq!(reopened.load_held_lettering(&rows[1]).unwrap(), None);
    }

    /// Held lettering whose file was altered, or that does not decode, is
    /// treated as gone: the caller looks under the box rather than failing.
    #[test]
    fn held_lettering_that_fails_its_digest_or_decode_reads_as_none() {
        let scratch = Scratch::new("held-lettering-damaged");
        let mut job = a_job(&scratch);
        let mut lettering = Mask::empty(Rect::new(10, 10, 12, 6));
        lettering.set(10, 10, true);
        job.leave_untouched_rows(vec![PendingUntouched {
            row: RegionUntouched { source_idx: 0, bbox: lettering.bounds, reason: "review.reason.unassignedMask".into(),
                inside_bubble: Some(false), lettering: None },
            lettering: Some((lettering, 1.0)),
        }]).unwrap();
        let reopened = Job::open(job.path()).unwrap();
        let row = reopened.project.regions_untouched[0].clone();
        let file = reopened.sidecar().join(&row.lettering.as_ref().unwrap().mask_ref);

        std::fs::write(&file, b"altered").unwrap();
        assert_eq!(reopened.load_held_lettering(&row).unwrap(), None, "a digest mismatch failed the read");

        // Bytes that match a digest but are not a mask.
        let garbage = b"not a mask".to_vec();
        let name = format!("candidates/{}.mask", sha256_hex(&garbage));
        std::fs::write(reopened.sidecar().join(&name), &garbage).unwrap();
        let mut undecodable = row.clone();
        undecodable.lettering.as_mut().unwrap().mask_ref = name;
        assert_eq!(reopened.load_held_lettering(&undecodable).unwrap(), None, "a decode failure failed the read");
    }

    /// A manifest written before rows kept lettering opens, and its rows look
    /// under the box as they did; a row that names no lettering writes none.
    #[test]
    fn an_untouched_row_without_lettering_reads_and_writes_as_before() {
        let old: RegionUntouched = serde_json::from_value(serde_json::json!({
            "source_idx": 0, "bbox": {"x": 1, "y": 2, "w": 3, "h": 4},
            "reason": "review.reason.isolatedMask", "inside_bubble": true,
        })).unwrap();
        assert_eq!(old.lettering, None);
        assert_eq!(old.inside_bubble, Some(true));
        let written = serde_json::to_value(&old).unwrap();
        assert!(written.get("lettering").is_none());

        let scratch = Scratch::new("held-lettering-legacy");
        let mut job = a_job(&scratch);
        job.leave_untouched_rows(vec![old]).unwrap();
        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(reopened.load_held_lettering(&reopened.project.regions_untouched[0]).unwrap(), None);
        assert!(!reopened.sidecar().join("candidates").exists());
    }

    /// Opening removes evidence files no row names and keeps every file a
    /// detection's group or a patch's group snapshot names.
    #[test]
    fn opening_sweeps_only_unnamed_evidence_files() {
        let scratch = Scratch::new("evidence-orphans");
        let mut job = a_job(&scratch);
        let blob = |w: u32| {
            let mask = vec![255u8; w as usize * 4];
            crate::text_groups::group(&crate::text_groups::Inputs::new(w, 4, Some(&mask)))
                .unwrap()
                .evidence(Some(&mask))
                .unwrap()
        };
        let (detected, patched, orphan) = (blob(4), blob(5), blob(6));
        for evidence in [&detected, &patched, &orphan] {
            job.write_evidence(evidence).unwrap();
        }
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let mut record = a_detection(&mut job, mask.bounds);
        let grouping = crate::text_groups::group(&crate::text_groups::Inputs::new(4, 4, Some(&[255u8; 16]))).unwrap();
        record.group = Some(grouping.record(&grouping.groups[0], Some(&detected)));
        job.store_detection(record, &mask, &mask).unwrap();
        let mut patch = a_patch("c1-p001-r0");
        patch.provenance.params_snapshot = serde_json::json!({ "group": { "evidence": patched.reference() } });
        job.complete_region(0, &patch, None).unwrap();
        let unrelated = job.sidecar().join("evidence/notes.txt");
        std::fs::write(&unrelated, b"keep").unwrap();
        drop(Job::open(job.path()).unwrap());
        assert!(job.sidecar().join(detected.reference()).exists());
        assert!(job.sidecar().join(patched.reference()).exists());
        assert!(!job.sidecar().join(orphan.reference()).exists(), "an unnamed evidence file survived");
        assert!(unrelated.exists());
    }

    /// A read-only open reads what [`Job::open`] reads and changes nothing:
    /// no lock file appears beside the manifest, an orphaned detection file
    /// stays, and a write is refused before it touches anything.
    #[test]
    fn a_read_only_open_leaves_the_job_as_it_found_it() {
        let scratch = Scratch::new("read-only-open");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let record = a_detection(&mut job, mask.bounds);
        let id = record.id.clone();
        job.store_detection(record, &mask, &mask).unwrap();
        let path = job.path().to_path_buf();
        drop(job);
        let _ = std::fs::remove_file(lock::lock_path(&path));
        let stray = sidecar_dir(&path).join("detections/orphan.mask");
        std::fs::write(&stray, b"orphan").unwrap();
        let manifest = std::fs::read(&path).unwrap();

        let mut reader = Job::open_read_only(&path).unwrap();
        assert!(reader.load_detection(&id).unwrap().is_some());
        assert!(stray.exists(), "a read-only open swept");
        assert!(!lock::lock_path(&path).exists(), "a read-only open made a lock file");
        assert!(matches!(reader.remove_detection(&id), Err(StoreError::ReadOnly { .. })));
        assert!(matches!(reader.flush(), Err(StoreError::ReadOnly { .. })));
        reader.delete_detection_files(&reader.project.detections.clone());
        assert_eq!(std::fs::read(&path).unwrap(), manifest);
        assert!(!lock::lock_path(&path).exists());
        assert!(Job::open(&path).unwrap().load_detection(&id).unwrap().is_some());
    }

    /// A writer may be between its detection masks and the manifest naming
    /// them whenever it holds the job, so an open that cannot take the job
    /// leaves the sweep for later.
    #[test]
    fn opening_does_not_sweep_while_another_thread_holds_the_job() {
        let scratch = Scratch::new("detection-sweep-held");
        let job = a_job(&scratch);
        let path = job.path().to_path_buf();
        let pending = job.sidecar().join("detections/pending.mask");
        std::fs::create_dir_all(pending.parent().unwrap()).unwrap();
        std::fs::write(&pending, b"pending").unwrap();
        let (held, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        let writer = {
            let path = path.clone();
            std::thread::spawn(move || {
                let _lock = lock::lock(&path).unwrap();
                held.0.send(()).unwrap();
                release.1.recv().unwrap();
            })
        };
        held.1.recv().unwrap();
        Job::open(&path).unwrap();
        assert!(pending.exists(), "swept under another thread's write");
        release.0.send(()).unwrap();
        writer.join().unwrap();
        Job::open(&path).unwrap();
        assert!(!pending.exists(), "an open with the job free did not sweep");
    }

    /// The lost update itself, inside one process: a job read before another
    /// save cannot write its older manifest over it. Nothing of it is written,
    /// the compatibility ink beside the patches included.
    #[test]
    fn a_job_read_before_another_save_cannot_overwrite_it() {
        let scratch = Scratch::new("stale-save");
        let first = a_job(&scratch);
        let path = first.path().to_path_buf();
        let mut earlier = Job::open(&path).unwrap();
        let mut later = Job::open(&path).unwrap();
        assert_eq!(earlier.generation(), later.generation());

        later.complete_region(0, &a_patch("later"), None).unwrap();
        assert_ne!(later.generation(), earlier.generation());
        let refused = earlier.complete_region(0, &a_patch("earlier"), None);
        assert!(matches!(refused, Err(StoreError::Stale { .. })), "{refused:?}");
        assert!(earlier.went_stale());
        assert!(!later.went_stale());
        assert!(!earlier.sidecar().join("earlier.ink").exists(), "a stale job wrote a sidecar");
        assert!(matches!(earlier.flush(), Err(StoreError::Stale { .. })));

        let saved = Job::open(&path).unwrap();
        assert_eq!(saved.project.patches.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), vec!["later"]);
        // The job that saved stands on its own save and goes on saving.
        later.complete_region(0, &a_patch("later-again"), None).unwrap();
        assert_eq!(Job::open(&path).unwrap().project.patches.len(), 2);
    }

    /// Any change counts, including one from a writer that knows nothing of
    /// this build: an older version, or a person with a text editor.
    #[test]
    fn a_manifest_changed_or_removed_by_anyone_is_not_overwritten() {
        let scratch = Scratch::new("stale-external");
        let job = a_job(&scratch);
        let path = job.path().to_path_buf();
        let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["app_version"] = serde_json::json!("0.0.1-older");
        std::fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        assert!(matches!(job.flush(), Err(StoreError::Stale { .. })));
        assert_eq!(Job::open(&path).unwrap().project.app_version, "0.0.1-older");

        let reopened = Job::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(reopened.flush(), Err(StoreError::Stale { .. })), "a deleted chapter came back");
        assert!(!path.exists());
    }

    /// The last look is taken after the new manifest is synced, right before
    /// the rename: a writer that skips the lock and saves while this one is
    /// writing its temporary file is not overwritten either, and the refused
    /// save leaves no temporary file behind.
    #[test]
    fn a_manifest_changed_while_the_save_is_staged_is_not_overwritten() {
        let scratch = Scratch::new("stale-staged");
        let job = a_job(&scratch);
        let path = job.path().to_path_buf();
        let refused = job.flush_staged(|| {
            let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            raw["app_version"] = serde_json::json!("0.0.1-racing");
            std::fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        });
        assert!(matches!(refused, Err(StoreError::Stale { .. })), "{refused:?}");
        assert!(job.went_stale());
        assert_eq!(Job::open(&path).unwrap().project.app_version, "0.0.1-racing");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "a refused save left {leftovers:?}");
    }

    /// Looking for a job that is not there leaves nothing behind: no lock
    /// file beside a chapter that has no manifest.
    #[test]
    fn opening_a_job_that_does_not_exist_makes_no_lock_file() {
        let scratch = Scratch::new("open-missing");
        let path = scratch.join("absent.mtclean");
        assert!(matches!(Job::open(&path), Err(StoreError::Io { .. })));
        assert!(!lock::lock_path(&path).exists(), "a missing job was given a lock file");
    }

    /// Creating is starting over, so it replaces whatever manifest is there.
    #[test]
    fn creating_replaces_an_existing_manifest() {
        let scratch = Scratch::new("create-over");
        let job = a_job(&scratch);
        let path = job.path().to_path_buf();
        let project = Project::new(path.parent().unwrap(), "0.2.0", StripMode::Single, &[]);
        let created = Job::create(&path, project).unwrap();
        assert_eq!(Job::open(&path).unwrap().project.app_version, "0.2.0");
        created.flush().unwrap();
    }

    #[test]
    fn exhausted_detection_counter_refuses_without_reusing_an_id() {
        let scratch = Scratch::new("detection-counter");
        let mut job = a_job(&scratch);
        job.project.counters.detections = u32::MAX;
        assert!(matches!(job.try_next_detection_id("c1-p001"), Err(StoreError::Malformed(_))));
        assert_eq!(job.project.counters.detections, u32::MAX);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let record = DetectedRegion {
            id: String::new(), source_idx: 0, bbox: mask.bounds, mask_ref: String::new(),
            inside: false, balloon_color: None, script: None, pick: "fill".into(),
            detector: "local".into(), created: 0, mask_sha256: String::new(), order: 0,
            review_state: None, fit: None, group: None,
            padding_from_seed: false,
            padding_px: 0,
        };
        assert!(matches!(job.replace_page_detection_results(0, "c1-p001",
            vec![LoadedDetection { record, mask: mask.clone(), ink: mask, evidence: None, base: None }], Vec::<RegionUntouched>::new()),
            Err(StoreError::Malformed(_))));
        assert!(Job::open(job.path()).unwrap().project.detections.is_empty());
    }

    #[test]
    fn replacing_detection_results_keeps_other_pages_and_removes_old_files_after_flush() {
        let scratch = Scratch::new("detection-replace");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let old = a_detection(&mut job, mask.bounds);
        let old_id = old.id.clone();
        job.store_detection(old, &mask, &mask).unwrap();
        let replacement = a_detection(&mut job, Rect::new(30, 30, 8, 8));
        job.replace_page_detection_results(0, "c1-p001", vec![LoadedDetection {
            record: replacement, mask: mask.clone(), ink: mask.clone(), evidence: None,
            base: None,
        }], vec![RegionUntouched { source_idx: 0, bbox: Rect::new(60, 60, 3, 3),
            reason: "review.reason.gateSkippedLowConfidence".into(), inside_bubble: None, lettering: None }]).unwrap();
        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(reopened.project.detections.len(), 1);
        assert_ne!(reopened.project.detections[0].id, old_id);
        assert!(!job.sidecar().join(format!("detections/{old_id}.mask")).exists());
        assert_eq!(reopened.project.regions_untouched.len(), 1);
    }

    /// A mask edit keeps the edited ids, writes their masks under fresh names
    /// with a matching digest, mints the created id, drops the removed record,
    /// and deletes the superseded files only once the manifest names the new
    /// ones. A second edit of the same detection moves on to the next name.
    #[test]
    fn a_mask_edit_rewrites_its_detections_under_fresh_files_in_one_write() {
        let scratch = Scratch::new("detection-mask-edit");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let (kept, gone) = (a_detection(&mut job, mask.bounds), a_detection(&mut job, Rect::new(30, 30, 4, 4)));
        let (kept_id, gone_id) = (kept.id.clone(), gone.id.clone());
        job.store_detection(kept, &mask, &mask).unwrap();
        job.store_detection(gone, &Mask::filled(Rect::new(30, 30, 4, 4)), &Mask::filled(Rect::new(30, 30, 4, 4)))
            .unwrap();
        let old = job.load_detection(&kept_id).unwrap().unwrap();
        let gone_record = job.project.detections[1].clone();

        let grown = Mask::filled(Rect::new(8, 8, 16, 10));
        let ink = Mask::filled(Rect::new(10, 10, 4, 4));
        let mut record = old.record.clone();
        record.bbox = grown.bounds;
        let new = Mask::filled(Rect::new(50, 50, 3, 3));
        let mut fresh = a_detection(&mut job, new.bounds);
        fresh.id = String::new();
        let created = job.rewrite_page_detections(0, "c1-p001",
            vec![LoadedDetection { record, mask: grown.clone(), ink: ink.clone(), evidence: None, base: None }],
            vec![LoadedDetection { record: fresh, mask: new.clone(), ink: new.clone(), evidence: None, base: None }],
            std::slice::from_ref(&gone_id)).unwrap();
        assert_eq!(created, vec!["c1-p001-d3".to_owned()]);

        let reopened = Job::open(job.path()).unwrap();
        let ids: Vec<_> = reopened.project.detections.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, [kept_id.as_str(), "c1-p001-d3"]);
        let edited = reopened.load_detection(&kept_id).unwrap().unwrap();
        assert_eq!(edited.record.mask_ref, format!("detections/{kept_id}.e1.mask"));
        assert_eq!(edited.record.ink_ref(), format!("detections/{kept_id}.e1.ink"));
        assert_eq!(edited.record.mask_sha256, sha256_hex(&buffers::encode_mask(&grown)));
        assert_eq!((edited.mask, edited.ink), (grown, ink.clone()));
        assert_eq!(edited.record.bbox, Rect::new(8, 8, 16, 10));
        assert_eq!(edited.record.fit, old.record.fit, "the caller's record fields were not kept");
        let made = reopened.load_detection("c1-p001-d3").unwrap().unwrap();
        assert_eq!((made.record.source_idx, made.mask), (0, new));
        for gone in [&old.record, &gone_record] {
            assert!(!job.sidecar().join(&gone.mask_ref).exists(), "{} survived", gone.mask_ref);
            assert!(!job.sidecar().join(gone.ink_ref()).exists(), "{} survived", gone.ink_ref());
        }

        let mut job = reopened;
        let again = job.load_detection(&kept_id).unwrap().unwrap();
        let smaller = Mask::filled(Rect::new(8, 8, 6, 6));
        job.rewrite_page_detections(0, "c1-p001", vec![LoadedDetection {
            record: again.record.clone(), mask: smaller.clone(), ink: smaller, evidence: None,
            base: None,
        }], Vec::new(), &[]).unwrap();
        let second = job.load_detection(&kept_id).unwrap().unwrap();
        assert_eq!(second.record.mask_ref, format!("detections/{kept_id}.e2.mask"));
        assert!(!job.sidecar().join(&again.record.mask_ref).exists());
    }

    /// A failed edit restores the job and leaves the manifest and the files it
    /// names alone. The half-written new files are orphans the next open
    /// sweeps. A removed or updated id that is not on the page is refused.
    #[test]
    fn a_failed_mask_edit_leaves_the_old_detections_in_place() {
        let scratch = Scratch::new("detection-mask-edit-fails");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let record = a_detection(&mut job, mask.bounds);
        let id = record.id.clone();
        job.store_detection(record, &mask, &mask).unwrap();
        let old = job.load_detection(&id).unwrap().unwrap();
        let before = std::fs::read(job.path()).unwrap();

        // The update is written, then minting the created id fails.
        let fresh = a_detection(&mut job, Rect::new(40, 40, 2, 2));
        job.project.counters.detections = u32::MAX;
        let bigger = Mask::filled(Rect::new(4, 4, 20, 20));
        let result = job.rewrite_page_detections(0, "c1-p001",
            vec![LoadedDetection { record: old.record.clone(), mask: bigger.clone(), ink: bigger, evidence: None, base: None }],
            vec![LoadedDetection { record: fresh, mask: mask.clone(), ink: mask.clone(), evidence: None, base: None }], &[]);
        assert!(matches!(result, Err(StoreError::Malformed(_))));
        assert_eq!(job.project.detections, vec![old.record.clone()]);
        assert_eq!(job.project.counters.detections, u32::MAX);
        assert_eq!(std::fs::read(job.path()).unwrap(), before, "the manifest was written");
        let orphan = job.sidecar().join(format!("detections/{id}.e1.mask"));
        assert!(orphan.exists());

        for (updated, removed) in [(Vec::new(), vec!["c1-p001-d99".to_owned()]),
            (vec![LoadedDetection { record: old.record.clone(), mask: mask.clone(), ink: mask.clone(), evidence: None, base: None }],
                vec![id.clone()])] {
            assert!(matches!(job.rewrite_page_detections(0, "c1-p001", updated, Vec::new(), &removed),
                Err(StoreError::Malformed(_))));
            assert_eq!(job.project.detections, vec![old.record.clone()]);
        }
        assert!(matches!(job.rewrite_page_detections(1, "c1-p002", Vec::new(), Vec::new(), std::slice::from_ref(&id)),
            Err(StoreError::Malformed(_))), "a detection on another page was removed");

        let reopened = Job::open(job.path()).unwrap();
        assert!(!orphan.exists(), "the orphan was not swept");
        let loaded = reopened.load_detection(&id).unwrap().unwrap();
        assert_eq!((loaded.record, loaded.mask, loaded.ink), (old.record, old.mask, old.ink));
    }

    #[test]
    fn a_declined_detection_becomes_its_untouched_row_and_a_deleted_one_goes() {
        let scratch = Scratch::new("detection-decline");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let ink = Mask::filled(Rect::new(10, 10, 6, 4));
        let mut first = a_detection(&mut job, Rect::new(9, 9, 10, 8));
        first.fit = first.fit.map(|fit| DetectionFit { scale: 0.5, ..fit });
        let second = a_detection(&mut job, Rect::new(30, 30, 4, 4));
        job.store_detection(first, &mask, &ink).unwrap();
        job.store_detection(second, &mask, &mask).unwrap();

        assert!(job.decline_detection("c1-p001-d0", "decline.reason.edgeEnergy").unwrap());
        assert_eq!(job.project.regions_untouched.len(), 1);
        let row = job.project.regions_untouched[0].clone();
        assert_eq!(row.bbox, Rect::new(9, 9, 10, 8));
        // What a clean the user asks for needs is kept: the lettering at its
        // fit scale. Not the balloon answer, which marks a held candidate.
        assert_eq!(row.inside_bubble, None);
        assert_eq!(job.load_held_lettering(&row).unwrap(), Some((ink.clone(), 0.5)));

        let removed = job.remove_detection("c1-p001-d1").unwrap().expect("there");
        assert_eq!(removed.bbox, Rect::new(30, 30, 4, 4));
        assert!(!job.sidecar().join(&removed.mask_ref).exists());
        assert!(!job.sidecar().join(removed.ink_ref()).exists());
        assert!(job.remove_detection("c1-p001-d1").unwrap().is_none());
        let reopened = Job::open(job.path()).unwrap();
        assert!(reopened.project.detections.is_empty());
        assert_eq!(reopened.project.regions_untouched.len(), 1);
        // The row names its lettering, so the reopen's sweep keeps the file.
        assert_eq!(reopened.load_held_lettering(&reopened.project.regions_untouched[0]).unwrap(), Some((ink, 0.5)));
    }

    #[test]
    fn a_detection_half_inside_a_patch_box_is_cleaned_already() {
        let scratch = Scratch::new("detection-covered");
        let mut job = a_job(&scratch);
        // a_patch covers (8, 8, 12, 10).
        job.complete_region(0, &a_patch("c1-p001-r0"), None).unwrap();
        assert!(job.detection_is_cleaned_already(0, Rect::new(9, 9, 6, 6)));
        // Exactly half inside.
        assert!(job.detection_is_cleaned_already(0, Rect::new(14, 8, 12, 10)));
        assert!(!job.detection_is_cleaned_already(0, Rect::new(15, 8, 12, 10)));
        assert!(!job.detection_is_cleaned_already(1, Rect::new(9, 9, 6, 6)));
    }

    /// A manifest written before detections existed has neither the list nor
    /// the counter. It opens with none, and a flush does not add either.
    #[test]
    fn a_manifest_without_detections_opens_and_is_written_without_them() {
        let scratch = Scratch::new("detection-absent");
        let job = a_job(&scratch);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        assert!(saved.get("detections").is_none());
        assert!(saved["counters"].get("detections").is_none());
        let reopened = Job::open(job.path()).unwrap();
        assert!(reopened.project.detections.is_empty());
        assert_eq!(reopened.project.counters.detections, 0);
    }

    #[test]
    fn a_stale_source_loses_its_detections_and_their_files() {
        let scratch = Scratch::new("detection-stale");
        let mut job = a_job(&scratch);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let record = a_detection(&mut job, Rect::new(9, 9, 10, 8));
        job.store_detection(record, &mask, &mask).unwrap();
        job.drop_stale(&[SourceState::Missing]).unwrap();
        assert!(job.project.detections.is_empty());
        assert!(!job.sidecar().join("detections/c1-p001-d0.mask").exists());
    }

    #[test]
    fn a_manifest_is_never_seen_half_written() {
        // The property temp-and-rename buys: at no point does the manifest path
        // hold anything but a complete file. Approximated here by checking that
        // the write goes through a temp path and that the destination parses
        // after every flush.
        let scratch = Scratch::new("atomic");
        let mut job = a_job(&scratch);
        for index in 0..8 {
            job.complete_region(0, &a_patch(&format!("r{index}")), None)
                .unwrap();
            let bytes = std::fs::read(job.path()).unwrap();
            serde_json::from_slice::<Project>(&bytes).expect("the manifest parsed mid-job");
        }
        let leftovers: Vec<_> = std::fs::read_dir(job.path().parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "a temp file was left behind");
    }

    #[test]
    fn a_resume_hashes_the_sources_and_drops_what_changed() {
        let scratch = Scratch::new("resume");
        let mut job = a_job(&scratch);
        job.complete_region(0, &a_patch("r0"), None).unwrap();
        assert_eq!(job.verify_sources(), vec![SourceState::Unchanged]);

        // Re-crop the page in another application, which is §6's worked
        // example: without this check it composites at stale coordinates and
        // the fidelity test still passes, because it compares against the new
        // source.
        let source = job.source_path(0).unwrap();
        let other = encode(&fixtures::by_name("rgb8").raster, Format::Png).unwrap();
        std::fs::write(&source, other).unwrap();

        let states = job.verify_sources();
        assert!(matches!(states[0], SourceState::Changed { .. }));
        let report = job.drop_stale(&states).unwrap();
        assert_eq!(report.sources, vec![0]);
        assert_eq!(report.dropped_patches, vec!["r0".to_string()]);
        assert!(job.project.patches.is_empty());
        assert!(
            Job::open(job.path()).unwrap().project.patches.is_empty(),
            "the drop was not flushed"
        );
    }

    #[test]
    fn a_deleted_source_is_missing_rather_than_changed() {
        let scratch = Scratch::new("missing");
        let mut job = a_job(&scratch);
        job.complete_region(0, &a_patch("r0"), None).unwrap();
        std::fs::remove_file(job.source_path(0).unwrap()).unwrap();

        let states = job.verify_sources();
        assert_eq!(states, vec![SourceState::Missing]);
        assert_eq!(
            job.drop_stale(&states).unwrap().dropped_patches,
            vec!["r0".to_string()]
        );
    }

    /// An untouched mtime is not evidence of an untouched file, and a moved one
    /// is not evidence of a changed one. The hash decides.
    #[test]
    fn an_mtime_alone_does_not_decide_staleness() {
        let scratch = Scratch::new("mtime");
        let job = a_job(&scratch);
        let source = job.source_path(0).unwrap();
        let bytes = std::fs::read(&source).unwrap();
        std::fs::remove_file(&source).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        assert_eq!(
            job.verify_sources(),
            vec![SourceState::Unchanged],
            "a new mtime read as a change"
        );
    }

    #[test]
    fn the_source_folder_is_refused_as_a_destination() {
        let scratch = Scratch::new("refuse");
        let job = a_job(&scratch);
        let raws = scratch.join("raws");
        assert!(matches!(
            job.output_refusal(&raws),
            Some(OutputRefusal::SourceDirectory { .. })
        ));
        assert_eq!(job.output_refusal(&scratch.join("raws_cleaned")), None);
        // Reached the long way round, which the lexical comparison alone would
        // miss.
        assert!(job.output_refusal(&scratch.join("out/../raws")).is_some());
    }

    #[test]
    fn a_file_whose_bytes_are_a_source_is_recognised_wherever_it_sits() {
        let scratch = Scratch::new("hash-refuse");
        let job = a_job(&scratch);
        let bytes = std::fs::read(job.source_path(0).unwrap()).unwrap();
        assert_eq!(job.project.source_with_hash(&sha256_hex(&bytes)), Some(0));
        assert_eq!(job.project.source_with_hash(&"0".repeat(64)), None);
    }

    #[test]
    fn the_default_output_directory_is_a_sibling_of_the_input() {
        assert_eq!(
            default_output_dir(Path::new("/scans/ch01")),
            PathBuf::from("/scans/ch01_cleaned")
        );
    }

    #[test]
    fn sources_are_stored_relative_so_a_job_survives_being_moved() {
        let scratch = Scratch::new("relative");
        let job = a_job(&scratch);
        assert_eq!(
            job.project.sources[0].rel_path,
            PathBuf::from("../raws/001.png")
        );

        // Move the pair, and the source still resolves.
        let moved = scratch.join("moved");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::rename(scratch.join("raws"), moved.join("raws")).unwrap();
        std::fs::rename(scratch.join("out"), moved.join("out")).unwrap();
        let reopened = Job::open(&moved.join("out/chapter.mtclean")).unwrap();
        assert_eq!(reopened.verify_sources(), vec![SourceState::Unchanged]);
    }

    /// §2's listing is the product - "a job that silently drops files is worse
    /// than one that cleans nothing" - and the listing is worthless if it dies
    /// with the session that made it.
    #[test]
    fn the_files_ingest_refused_survive_in_the_manifest() {
        let scratch = Scratch::new("input-report");
        let dir = scratch.join("raws");
        std::fs::create_dir_all(&dir).unwrap();
        let png = encode(&fixtures::by_name("l8").raster, Format::Png).unwrap();
        let other = encode(&fixtures::by_name("rgb8").raster, Format::Png).unwrap();
        let files: Vec<(PathBuf, Vec<u8>)> = vec![
            (dir.join("001.png"), png.clone()),
            (dir.join("001.tif"), other.clone()),
            (dir.join("002.png"), b"not an image at all".to_vec()),
            (dir.join(".DS_Store"), b"junk".to_vec()),
        ];
        let report = crate::ingest::ingest(
            files
                .iter()
                .map(|(path, bytes)| (path.as_path(), bytes.as_slice())),
        );

        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::from_ingest(
            manifest.parent().unwrap(),
            "0.1.0",
            StripMode::Single,
            &report,
        );
        let job = Job::create(&manifest, project).unwrap();

        let reopened = Job::open(job.path()).unwrap().project;
        assert_eq!(reopened.input_report.junk_skipped, 1);
        assert_eq!(
            reopened.input_report.duplicate_basenames,
            vec!["001".to_string()]
        );
        assert_eq!(
            reopened.input_report.skipped,
            vec![SkippedInput {
                file: "002.png".into(),
                reason: "input.skipReason.notAnImage".into()
            }],
            "a refused file left no trace in the manifest"
        );
        // The junk entry is counted, never listed: it is not a page anybody
        // expected to see, and naming `.DS_Store` in a skip list is noise.
        assert!(!reopened
            .input_report
            .skipped
            .iter()
            .any(|s| s.file == ".DS_Store"));
    }

    /// §6's "any incomplete job offers a resume" needs a manifest that can say
    /// a job is incomplete, and the offer has to be withdrawable.
    #[test]
    fn an_interrupted_job_says_so_and_can_stop_saying_so() {
        let scratch = Scratch::new("interrupted");
        let mut job = a_job(&scratch);
        assert_eq!(job.project.interrupted_at, None);

        job.mark_interrupted(Some(7)).unwrap();
        assert_eq!(
            Job::open(job.path()).unwrap().project.interrupted_at,
            Some(7)
        );

        job.mark_interrupted(None).unwrap();
        assert_eq!(Job::open(job.path()).unwrap().project.interrupted_at, None);
    }

    /// The distinction the field exists for. A page the detector looked at and
    /// found nothing on records nothing else at all - no patch, no untouched
    /// region, no counter above zero - so this is the only thing that tells it
    /// apart from a page nothing has ever opened.
    #[test]
    fn a_page_examined_and_found_empty_is_not_a_page_nobody_looked_at() {
        let scratch = Scratch::new("examined");
        let mut job = a_job(&scratch);
        assert!(job.project.examined.is_empty());

        job.mark_examined(0).unwrap();
        job.mark_examined(0).unwrap();

        let reopened = Job::open(job.path()).unwrap().project;
        assert_eq!(
            reopened.examined,
            vec![0],
            "a second pass doubled the record"
        );
        assert!(reopened.patches.is_empty());
        assert!(reopened.regions_untouched.is_empty());
        assert_eq!(reopened.counters, Counters::default());
    }

    #[test]
    fn a_source_that_changed_counts_as_unexamined_again() {
        let scratch = Scratch::new("examined-stale");
        let mut job = a_job(&scratch);
        job.mark_examined(0).unwrap();

        let source = job.source_path(0).unwrap();
        let other = encode(&fixtures::by_name("rgb8").raster, Format::Png).unwrap();
        std::fs::write(&source, other).unwrap();
        let states = job.verify_sources();
        job.drop_stale(&states).unwrap();

        assert!(
            Job::open(job.path()).unwrap().project.examined.is_empty(),
            "a re-cropped page stayed out of the queue with nothing on it"
        );
    }

    /// **A version-1 manifest opens, and that is what the bump to 2 was for.**
    ///
    /// The number went up so a v1 *build* refuses a manifest holding
    /// `"engine": "paint"` for its version rather than as an `unknown variant`
    /// deep in the body. Nothing about that requires this build to refuse the
    /// files it wrote last week: the v2 types only widened an enum, so a v1
    /// file decodes into them unchanged. Written against a file that really
    /// says `"version": 1` on disk rather than against the constant, because
    /// the constant is the thing that moved.
    #[test]
    fn a_version_one_manifest_still_opens_and_is_rewritten_at_the_legacy_floor() {
        let scratch = Scratch::new("version-one");
        let job = a_job(&scratch);
        let mut old: serde_json::Value =
            serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        old["version"] = serde_json::json!(1);
        std::fs::write(job.path(), serde_json::to_vec_pretty(&old).unwrap()).unwrap();
        assert!(
            std::fs::read_to_string(job.path())
                .unwrap()
                .contains("\"version\": 1"),
            "the fixture has to actually be a v1 file"
        );

        // Opening raises the number in memory; the file on disk is untouched
        // until something writes it.
        let reopened = Job::open(job.path()).expect("a v1 manifest is still readable");
        assert_eq!(reopened.project.version, 3);

        // And the next flush is what upgrades the file - no migration pass, no
        // conversion step, and nothing the caller has to remember to do. This is
        // the half that was missing: `flush` writes `project.version` verbatim,
        // so a v1 chapter that gained a painted patch was being written back as
        // a v1 file carrying `"engine": "paint"`.
        reopened.flush().unwrap();
        let text = std::fs::read_to_string(job.path()).unwrap();
        assert!(
            text.contains("\"version\": 3"),
            "the flush did not rewrite the version"
        );
        assert_eq!(
            Job::open(job.path()).unwrap().project.version,
            3
        );
    }

    #[test]
    fn legacy_cloud_json_migrates_without_inventing_history_or_writing_on_read() {
        for version in [1, 2, 3] {
            let scratch = Scratch::new(&format!("legacy-cloud-v{version}"));
            let mut job = a_job(&scratch);
            job.complete_region(0, &a_patch("legacy"), None).unwrap();
            if version >= 2 {
                for (id, engine) in [("paint", Engine::Paint), ("clone", Engine::Clone)] {
                    let mut patch = a_patch(id);
                    patch.provenance.engine = engine;
                    job.complete_region(0, &patch, None).unwrap();
                }
            }
            let mut fixture: serde_json::Value =
                serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
            fixture["version"] = serde_json::json!(version);
            // These fields did not exist before v4 and must not turn a legacy
            // record into an implicit text-shaped plan during migration.
            fixture.as_object_mut().unwrap().remove("text_shape_plans");
            for record in fixture["patches"].as_array_mut().unwrap() {
                record.as_object_mut().unwrap().remove("geometry_policy");
                record
                    .as_object_mut()
                    .unwrap()
                    .remove("text_shape_plan_identity");
            }
            fixture["patches"][0]["engine"] = serde_json::json!("cloud");
            fixture["patches"][0]["provenance"]["engine"] = serde_json::json!("cloud");
            // This is the exact old JSON shape, not serialization of the new
            // CloudRecord with invented defaults for its additional fields.
            fixture["patches"][0]["provenance"]["cloud"] = serde_json::json!({
                "provider": "legacy-provider", "model": "legacy-model",
                "request_id": "legacy-request", "tier": "standard", "cost": 0.014
            });
            let before = serde_json::to_vec_pretty(&fixture).unwrap();
            std::fs::write(job.path(), &before).unwrap();
            let reopened = Job::open(job.path()).unwrap();
            assert_eq!(std::fs::read(job.path()).unwrap(), before);
            assert_eq!(reopened.project.version, 3);
            let record = &reopened.project.patches[0];
            assert!(reopened.project.text_shape_plans.is_empty());
            assert!(reopened
                .project
                .patches
                .iter()
                .all(|patch| patch.geometry_policy == GeometryPolicy::Legacy));
            assert_eq!(record.engine, Engine::Cloud);
            assert_eq!(record.provenance.engine, Engine::Cloud);
            let cloud = record.provenance.cloud.as_ref().unwrap();
            assert_eq!(cloud.provider, "legacy-provider");
            assert_eq!(cloud.model, "legacy-model");
            assert_eq!(cloud.cost, Some(0.014));
            assert_eq!(cloud.tier.as_deref(), Some("standard"));
            assert_eq!(cloud.profile_id, None);
            assert_eq!(cloud.job_id, None);
            assert_eq!(cloud.attempt_id, None);
            assert_eq!(cloud.recipe_id, None);
            assert_eq!(cloud.model_revision, None);
            assert_eq!(cloud.duration_ms, None);
            reopened.flush().unwrap();
            let upgraded = Job::open(job.path()).unwrap();
            assert_eq!(upgraded.project.patches, reopened.project.patches);
            let saved: serde_json::Value =
                serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
            assert_eq!(saved["version"], 3);
            assert_eq!(
                saved["patches"][0]["provenance"]["cloud"],
                fixture["patches"][0]["provenance"]["cloud"]
            );
            if version >= 2 {
                assert_eq!(upgraded.project.patches[1].engine, Engine::Paint);
                assert_eq!(upgraded.project.patches[2].engine, Engine::Clone);
            }
        }
    }

    #[test]
    fn v3_cloud_patch_roundtrips_explicit_null_cost_and_provenance() {
        let scratch = Scratch::new("v3-cloud-null-cost");
        let mut job = a_job(&scratch);
        let mut patch = a_patch("remote");
        patch.provenance.engine = Engine::Flux;
        patch.provenance.cloud = Some(
            serde_json::from_value(serde_json::json!({
                "provider": "modal", "profile_id": "test-profile", "job_id": "test-job",
                "request_id": "test-request", "attempt_id": "test-attempt",
                "recipe_id": "test-recipe", "model": "test-model",
                "model_revision": "test-immutable-revision", "cost": null, "duration_ms": 120
            }))
            .unwrap(),
        );
        job.complete_region(0, &patch, None).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        assert_eq!(saved["version"], 3);
        let cloud_json = &saved["patches"][0]["provenance"]["cloud"];
        assert!(cloud_json.as_object().unwrap().contains_key("cost"));
        assert!(cloud_json["cost"].is_null());
        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(reopened.project.patches[0].provenance, patch.provenance);
        assert_eq!(reopened.project.patches[0].engine, Engine::Flux);
        assert_eq!(
            reopened
                .load_patch(&reopened.project.patches[0])
                .unwrap()
                .pixels
                .data,
            patch.pixels.data
        );
    }

    /// A patch made by either hand tool survives the manifest, under the rung
    /// id its `rung_key` names. This is the value that could not exist before
    /// the bump and is the reason for it.
    #[test]
    fn a_painted_patch_round_trips_through_the_manifest() {
        let scratch = Scratch::new("paint-record");
        let mut job = a_job(&scratch);
        for (index, engine) in [Engine::Paint, Engine::Clone].into_iter().enumerate() {
            let mut patch = a_patch(&format!("r{index}"));
            patch.provenance.engine = engine;
            job.complete_region(0, &patch, None).unwrap();
        }

        let text = std::fs::read_to_string(job.path()).unwrap();
        assert!(text.contains("\"engine\": \"paint\""), "{text}");
        assert!(text.contains("\"engine\": \"clone\""), "{text}");

        let reopened = Job::open(job.path()).unwrap();
        let engines: Vec<Engine> = reopened
            .project
            .patches
            .iter()
            .map(|record| record.provenance.engine)
            .collect();
        assert_eq!(engines, vec![Engine::Paint, Engine::Clone]);
    }

    #[test]
    fn a_manifest_from_a_later_version_is_refused_rather_than_half_read() {
        let scratch = Scratch::new("version");
        let job = a_job(&scratch);
        let mut future: serde_json::Value =
            serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        future["version"] = serde_json::json!(FORMAT_VERSION + 1);
        std::fs::write(job.path(), serde_json::to_vec(&future).unwrap()).unwrap();
        assert!(matches!(
            Job::open(job.path()),
            Err(StoreError::Version { .. })
        ));
    }

    #[test]
    fn an_unknown_geometry_policy_is_not_migrated_to_legacy() {
        let scratch = Scratch::new("unknown-geometry-policy");
        let mut job = a_job(&scratch);
        job.complete_region(0, &a_patch("r0"), None).unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        manifest["patches"][0]["geometry_policy"] = serde_json::json!("future_mode");
        std::fs::write(job.path(), serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(matches!(Job::open(job.path()), Err(StoreError::Json(_))));
    }

    /// The version has to be read **before** the shape, or it is only ever
    /// consulted for the files that did not need it. A later version whose
    /// layout also moved is the ordinary case - that is what a version is for -
    /// and read in the other order it arrives as "not readable as JSON" about a
    /// file that is perfectly good JSON.
    #[test]
    fn a_later_version_is_recognised_even_when_its_shape_is_unreadable() {
        let scratch = Scratch::new("version-shape");
        let job = a_job(&scratch);
        std::fs::write(
            job.path(),
            serde_json::json!({ "version": FORMAT_VERSION + 1, "whatever": [1, 2, 3] }).to_string(),
        )
        .unwrap();

        match Job::open(job.path()).map(|_| ()) {
            Err(StoreError::Version { expected, found }) => {
                assert_eq!((expected, found), (FORMAT_VERSION, FORMAT_VERSION + 1));
            }
            other => panic!("a later version was refused as a malformed file: {other:?}"),
        }
    }

    /// `strip.splits` changed element type - `u32` to
    /// [`crate::strip::Split`] - inside version 1, and the field's own
    /// documentation is the justification. This is the fact that justification
    /// rests on: no build has ever written a populated `splits`, so the only
    /// value the change can meet on disk is `[]`, which decodes the same way
    /// under both types. A populated one from a hypothetical older writer is
    /// refused loudly rather than silently misread.
    #[test]
    fn nothing_writes_a_populated_splits_and_an_old_shaped_one_is_refused_loudly() {
        let scratch = Scratch::new("splits-shape");
        let job = a_job(&scratch);
        assert!(
            job.project.strip.splits.is_empty(),
            "a fresh project writes no splits"
        );

        let written = std::fs::read_to_string(job.path()).unwrap();
        assert!(written.contains(r#""splits": []"#), "{written}");
        assert!(Job::open(job.path()).is_ok());

        // The one manifest the change could have been wrong about, if a writer
        // for it had ever existed.
        std::fs::write(
            job.path(),
            written.replace(r#""splits": []"#, r#""splits": [2000]"#),
        )
        .unwrap();
        assert!(
            matches!(Job::open(job.path()), Err(StoreError::Json(_))),
            "a row of the old shape was accepted, or dropped, instead of refused"
        );
    }
    #[test]
    fn orientation_all_eight_preserve_native_edits_and_legacy_buffers() {
        use crate::image::orientation::Orientation;
        for value in 1..=8 {
            let scratch=Scratch::new(&format!("orientation-{value}"));
            let mut job=a_job(&scratch);
            let mut source=job.project.sources[0].clone(); source.orientation=Orientation(value);
            let mut native=a_patch("legacy");
            native.mask.bits[3]=0; native.mask.bits[5]=37;
            native.ink.bits[1]=9;
            for (i,byte) in native.pixels.data.iter_mut().enumerate(){*byte=(i%251) as u8;}
            job.complete_region(0,&native,None).unwrap();
            job.project.sources[0].orientation=Orientation(value);
            let buffer_ref=job.project.patches[0].buffer_ref.clone();
            let original_buffers=std::fs::read(job.sidecar().join(&buffer_ref)).unwrap();
            let visible=job.display_patches_on_page(0).unwrap().remove(0);
            assert_eq!(visible.mask,source.orientation.mask(&native.mask,source.w,source.h));
            assert_eq!(orientation::display_bbox(&job.project,&job.project.patches[0]),visible.mask.bounds);
            let mut edited=visible.clone();edited.id="new".into();edited.order=4;
            job.complete_display_region(0,&edited,None).unwrap();
            let reopened=Job::open(job.path()).unwrap();
            let roundtrip=reopened.native_patches_on_page(0).unwrap().into_iter().find(|p|p.id=="new").unwrap();
            assert_eq!(roundtrip.mask,native.mask);assert_eq!(roundtrip.ink,native.ink);
            assert_eq!(roundtrip.pixels.data,native.pixels.data);
            assert_eq!(std::fs::read(job.sidecar().join(&buffer_ref)).unwrap(),original_buffers);
            assert_eq!(reopened.load_display_patch(&reopened.project.patches[1]).unwrap().mask,visible.mask);
        }
    }

    #[test]
    fn oriented_text_shape_plans_keep_exact_support_and_native_patch_parts() {
        let scratch=Scratch::new("oriented-text-shape");let mut job=a_job(&scratch);
        job.project.sources[0].orientation=crate::image::orientation::Orientation(6);
        let source=job.project.sources[0].clone();let (w,h)=source.orientation.size(source.w,source.h);
        let mut plan=job.plan_fixture("oriented-text",1,2);plan.reading_context=Rect::new(0,0,w,h);
        let prepared=plan.prepare(w,h).unwrap();job.store_text_shape_plan(0,&plan,&prepared).unwrap();
        let mut patch=a_patch("oriented-text");patch.mask=prepared.write_support.to_mask();patch.ink=patch.mask.clone();
        patch.pixels.width=patch.mask.bounds.w;patch.pixels.height=patch.mask.bounds.h;patch.pixels.data=vec![199;patch.pixels.stride()*patch.pixels.height as usize];
        job.complete_text_shape_region(0,&patch,&prepared.identity.identity_sha256,None).unwrap();
        let opened=Job::open(job.path()).unwrap();
        assert_eq!(opened.project.version,5);assert!(opened.project.text_shape_plans[0].display_coordinates);
        assert_eq!(opened.load_patch(&opened.project.patches[0]).unwrap().mask,patch.mask);
        let native=opened.native_patches_on_page(0).unwrap().remove(0);
        assert_eq!(source.orientation.mask(&native.mask,source.w,source.h),patch.mask);
        assert_eq!(opened.load_text_shape_plan("oriented-text").unwrap().unwrap().1,prepared);
    }

    #[test]
    fn oriented_layers_keep_placement_opacity_and_revision_undo() {
        use crate::image::orientation::Orientation;
        for value in 2..=8 {
            let scratch=Scratch::new(&format!("orientation-layer-{value}"));
            let mut job=a_job(&scratch); job.project.sources[0].orientation=Orientation(value);
            let edit=a_patch("paint-layer"); job.complete_display_region(0,&edit,None).unwrap();
            let original=job.project.patches[0].clone();
            job.project.patches[0].provenance.params_snapshot["layer"] = serde_json::json!({"offsetX":3,"offsetY":4,"rotation":90.0,"opacity":61});
            job.flush().unwrap();
            let mut expected=edit.clone(); expected.provenance=job.project.patches[0].provenance.clone(); let expected=expected.presented();
            let actual=job.load_display_patch(&job.project.patches[0]).unwrap();
            assert_eq!(actual.mask,expected.mask,"orientation {value}");
            assert_eq!(actual.pixels.data,expected.pixels.data,"orientation {value}");
            assert_eq!(actual.layer_style().opacity,61);
            assert_eq!(orientation::display_bbox(&job.project,&job.project.patches[0]),expected.mask.bounds);
            let native=job.native_patches_on_page(0).unwrap().remove(0); let source=&job.project.sources[0];
            let displayed=source.orientation.patch(&native,source.w,source.h);
            assert_eq!(displayed.mask,expected.mask); assert_eq!(displayed.pixels.data,expected.pixels.data);
            let mut replacement=edit.clone(); replacement.pixels.data.fill(47); job.complete_display_region(0,&replacement,None).unwrap();
            assert!(job.project.legacy_patch_revisions.iter().any(|r|r.buffer_ref==original.buffer_ref));
            job.project.patches[0]=original;job.flush().unwrap();let reopened=Job::open(job.path()).unwrap();
            assert_eq!(reopened.load_display_patch(&reopened.project.patches[0]).unwrap().pixels.data,edit.pixels.data);
        }
    }

    #[test]
    fn orientation_grouped_seam_survives_reorder_undo_and_interrupted_replacement() {
        use crate::image::orientation::Orientation;
        let scratch=Scratch::new("orientation-seam");let mut job=a_job(&scratch);
        job.project.strip.mode=StripMode::Longstrip;
        job.project.sources.push(job.project.sources[0].clone());job.project.strip.order=vec![0,1];
        job.project.sources[0].orientation=Orientation(6);
        let display=orientation::strip(&job.project,true);let first=display.pages()[0];
        let mut edit=a_patch("seam");edit.mask=Mask::filled(Rect::new(8,first.height as i64-4,12,10));
        edit.mask.bits[0]=0;edit.mask.bits[5]=37;edit.ink=edit.mask.clone();
        job.complete_display_region(0,&edit,None).unwrap();
        let before=[job.native_patches_on_page(0).unwrap(),job.native_patches_on_page(1).unwrap()];
        assert_eq!(job.project.patches[0].native_parts.len(),2);
        let manifest=std::fs::read(job.path()).unwrap();
        let mut replacement=edit.clone();replacement.pixels.data.fill(23);
        job.complete_display_region(0,&replacement,None).unwrap();
        // Simulate death before replacing the manifest: old content-addressed
        // buffers are still valid even though new part files have been staged.
        std::fs::write(job.path(),manifest).unwrap();job=Job::open(job.path()).unwrap();
        assert_eq!(job.native_patches_on_page(0).unwrap()[0].pixels.data,before[0][0].pixels.data);
        job.project.strip.order.reverse();job.flush().unwrap();
        assert_eq!(job.native_patches_on_page(1).unwrap()[0].mask,before[0][0].mask);
        assert_eq!(job.native_patches_on_page(0).unwrap()[0].mask,before[1][0].mask);
        job.project.patches[0].visible=false;job.flush().unwrap();
        let mut reopened=Job::open(job.path()).unwrap();
        assert!(reopened.native_patches_on_page(0).unwrap().iter().all(|p|!p.visible));
        reopened.project.patches[0].visible=true;reopened.flush().unwrap();
        assert!(Job::open(job.path()).unwrap().native_patches_on_page(0).unwrap()[0].visible);
    }

    #[test]
    fn orientation_legacy_manifest_reads_exif_without_rewriting_source_or_patch() {
        let scratch=Scratch::new("orientation-legacy");let mut job=a_job(&scratch);
        let source=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/color-reference/orientation-6.jpg");
        let bytes=std::fs::read(source).unwrap();let path=job.source_path(0).unwrap();std::fs::write(&path,&bytes).unwrap();
        let header=crate::image::foreign::jpeg_header(&bytes).unwrap();job.project.sources[0].w=header.width;job.project.sources[0].h=header.height;
        job.complete_region(0,&a_patch("legacy"),None).unwrap();
        let mut value:serde_json::Value=serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        value["version"]=serde_json::json!(2);value["sources"][0].as_object_mut().unwrap().remove("orientation");
        std::fs::write(job.path(),serde_json::to_vec(&value).unwrap()).unwrap();
        let opened=Job::open(job.path()).unwrap();assert_eq!(opened.project.sources[0].orientation.0,6);
        assert_eq!(std::fs::read(path).unwrap(),bytes);assert_eq!(opened.load_patch(&opened.project.patches[0]).unwrap().mask,a_patch("legacy").mask);
    }

    #[test]
    fn orientation_legacy_seam_freezes_native_layout_before_reorder() {
        let scratch=Scratch::new("orientation-legacy-reorder");let mut job=a_job(&scratch);
        job.project.strip.mode=StripMode::Longstrip;job.project.sources.push(job.project.sources[0].clone());job.project.strip.order=vec![0,1];
        let mut old=a_patch("old-seam");old.mask.bounds.y=job.project.sources[0].h as i64-4;old.ink=old.mask.clone();
        job.complete_region(0,&old,None).unwrap();
        job.project.sources[0].orientation=crate::image::orientation::Orientation(6);job.flush().unwrap();
        let mut json:serde_json::Value=serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();json["patches"][0].as_object_mut().unwrap().remove("native_order");
        std::fs::write(job.path(),serde_json::to_vec(&json).unwrap()).unwrap();job=Job::open(job.path()).unwrap();
        let before=[job.native_patches_on_page(0).unwrap(),job.native_patches_on_page(1).unwrap()];
        job.project.strip.order.reverse();job.flush().unwrap();let reopened=Job::open(job.path()).unwrap();
        for (source, patches) in before.iter().enumerate() {
            let after=reopened.native_patches_on_page(1-source).unwrap();
            assert_eq!(after[0].mask,patches[0].mask);assert_eq!(after[0].pixels.data,patches[0].pixels.data);
        }
        assert_eq!(reopened.load_patch(&reopened.project.patches[0]).unwrap().mask,old.mask);
    }

    #[test]
    fn installed_v3_cloud_chapter_preserves_extensions_revisions_and_unknown_cost() {
        let scratch=Scratch::new("installed-v3-cloud");let mut job=a_job(&scratch);
        let patch=a_patch("cloud-region");job.complete_region(0,&patch,None).unwrap();
        let mut value:serde_json::Value=serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        value["version"]=serde_json::json!(3);
        value["sources"][0]["future_source_field"]=serde_json::json!({"exact":17});
        value["patches"][0]["future_patch_field"]=serde_json::json!({"revision":3});
        value["patches"][0]["provenance"]["cloud"]=serde_json::json!({"provider":"modal","model":"test-model","request_id":"test-request","cost":null,"profile_id":"test-profile","job_id":"test-job","attempt_id":"test-attempt","recipe_id":"test-recipe","model_revision":"test-revision"});
        std::fs::write(job.path(),serde_json::to_vec(&value).unwrap()).unwrap();
        let opened=Job::open(job.path()).unwrap();assert_eq!(opened.project.sources.len(),1);assert_eq!(opened.project.patches.len(),1);
        assert_eq!(opened.load_patch(&opened.project.patches[0]).unwrap().ink,patch.ink);
        assert_eq!(opened.project.patches[0].provenance.cloud.as_ref().unwrap().cost,None);
        opened.flush().unwrap();let saved:serde_json::Value=serde_json::from_slice(&std::fs::read(job.path()).unwrap()).unwrap();
        for key in ["legacy_patch_revisions","detections","denoised","detected"] {assert_eq!(saved[key],value[key],"lost {key}");}
        assert_eq!(saved["sources"][0]["future_source_field"],value["sources"][0]["future_source_field"]);
        assert_eq!(saved["patches"][0]["future_patch_field"],value["patches"][0]["future_patch_field"]);
        assert_eq!(saved["patches"][0]["provenance"]["cloud"],value["patches"][0]["provenance"]["cloud"]);
        assert_eq!(saved["version"],3);
        let mut reopened=Job::open(job.path()).unwrap();
        reopened.complete_region(0,&patch,None).unwrap();
        let after=serde_json::to_value(&reopened.project).unwrap();
        assert_eq!(after["patches"][0]["future_patch_field"],value["patches"][0]["future_patch_field"]);
    }

}
