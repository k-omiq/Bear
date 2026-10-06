//! Text groups: the shared grouping and mask contract.
//!
//! Detection answers four different questions, and before this module one
//! type answered all of them. They are kept apart here, by type:
//!
//! | Layer | Type | What it is | Editable |
//! |---|---|---|---|
//! | Raw evidence | [`PixelComponent`], [`DetectorBox`], the page mask | What the models returned: 8-connected islands of the lettering mask, and every detector box above the model's own score floor, bubble boxes included | Never |
//! | Cleaning unit | [`TextGroup`] | One logical text block (or a coherent set of blocks in one balloon): the union of its glyph pixels | Yes, as a whole |
//! | Render job | one [`TextGroup`] (or one [`SplitPart`] of it) | The crop a renderer is asked to reconstruct | - |
//! | Layer | `DetectedRegion` / patch record, keyed by the region id | What the user sees and reviews | Yes |
//!
//! ## The contract
//!
//! 1. **Evidence is preserved, never rewritten.** The lettering mask a model
//!    returned, every raw detector box (Ogkalu bubble boxes included, which
//!    nothing stored before) and the component -> group assignment are written
//!    to a per-page evidence file ([`EvidenceBlob`], `evidence/<sha256>.mtev`
//!    beside the manifest). A group only *refers* to components by id; the
//!    exact lettering of any group can be rebuilt from that file
//!    ([`EvidenceFile::lettering`]) and is checked against
//!    [`GroupRecord::lettering_sha256`].
//! 2. **One group is one cleaning job and one layer by default.** A group
//!    becomes exactly one stored detection (`{page}-d{n}`) or one one-pass
//!    patch (`{page}-r{n}`). Region ids keep their old format, so journals,
//!    consent digests and cloud batching, all keyed by region id, are unchanged.
//!    [`GroupRecord::id`] is the group's exact-evidence identity: `tg-` + 12
//!    hex of a digest over the page it is on ([`Inputs::scope`]), the exact
//!    pixels of each of its components and ink islands, and its anchor, all
//!    in page coordinates, with a disambiguator should two groups on a page
//!    have identical evidence. It
//!    is independent of the region id counter and unique within a page's
//!    grouping, but stable only for identical evidence: a re-detection whose
//!    mask or boxes differ by one pixel names a different group.
//! 3. **The lettering mask is the union of the group's glyph pixels.** Never a
//!    solid rectangle, never the balloon. A text box whose group has mask
//!    lettering under 1% of its anchor (none at all, when no pixel model ran)
//!    also takes a bounded ink estimate: the Otsu ink population of its tight
//!    tier, refused above 45% of the box, less every mask pixel and every
//!    pixel an earlier group in reading order took, and refused outright when
//!    that leaves under half the box's ink (the rest is another group's, and
//!    the remainder is halo). No pixel seeds two groups. The estimate is
//!    flagged ([`TextGroup::estimated`]) and stored in the evidence file's
//!    owner plane, so it rebuilds exactly like model pixels. It seeds the fit;
//!    the fitted removal mask (`detections/<id>.mask`) and the fitted ink
//!    (`.ink`) are what a clean writes through, exactly as before. Padding, feather and context are
//!    the mask stage's business and are not decided here.
//! 4. **Provenance travels with the group.** [`GroupRecord`] names the models
//!    ([`ModelUse`]: model, local or cloud, spatial preprocessing), the
//!    contributing detector boxes with their raw rectangles and scores, the
//!    bubble it was confined to and its explicit review reasons. The chain
//!    source mask -> component ids -> group id -> region id -> request is
//!    therefore traceable end to end.
//! 5. **Review reasons are explicit and evidence-backed** ([`ReviewReason`]).
//!    Nothing here invents a confidence score from SAM components. A group
//!    with no reason is not flagged.
//! 6. **The mask decides what is lettering; boxes label it.** Lettering no
//!    text box claimed is still a cleaning group. Its record keeps why
//!    ([`ReviewReason::UnassignedComponent`]), which is provenance and not a
//!    review flag ([`GroupRecord::review_key`]). It is outside text unless
//!    [`crate::balloon::in_bubble`] puts it in one (a balloon box holds it,
//!    or closed paper surrounds it), so the run's outside-text choice and the gate decide
//!    whether it is cleaned, as they do for a `text_free` box. A [`Disposition::Candidate`] group is a box too weak to clean on its
//!    say alone: listed (a held, untouched row with its reason), never
//!    cleaned automatically.
//! 7. **Unusually large groups are split only to meet a model limit**
//!    ([`Inputs::max_side`], on both axes). Each component and ink island
//!    belongs to exactly one part (ownership by centre along the axis being
//!    cut); parts may overlap as crops, never as lettering. A part's id is the
//!    parent id with `-p{k}`, and [`SplitPart::parent`] names the parent.
//! 8. **Deterministic.** Every pass sorts before it merges and uses
//!    union-find with the lowest index as root; nothing iterates a hash map.
//!    The same evidence in any input order produces the same groups and ids
//!    (box ids `rt-NNNN` / `ctd-NNNN` name input positions, as the analysis
//!    view always has, but no grouping decision depends on them).
//!
//! ## Precedence
//!
//! - **Pixels**: SAM-TS-L's mask when SAM is selected; it is lettering
//!   segmentation. Without SAM, CTD's segmentation at its mask threshold
//!   ([`crate::detect::MASK_THRESHOLD`]) is the pixel evidence
//!   ([`Inputs::pixel_model`], component ids `seg-NNNNN` rather than
//!   `sam-NNNNN`). With neither, every group is a text box and its lettering
//!   is its ink estimate.
//! - **Grouping anchors**: Ogkalu `text_bubble` / `text_free` boxes first,
//!   then CTD boxes. A CTD box that duplicates an Ogkalu text box joins it; a
//!   CTD box Ogkalu missed anchors its own group. Anchors that overlap by more
//!   than half the smaller box, that were cut by a model tile seam, or that sit
//!   close together in one balloon become one group.
//! - **Walls**: Ogkalu `bubble` boxes at or above [`BUBBLE_SCORE`]. Anchors in
//!   different balloons never merge, and a glyph in a balloon is never
//!   attached to an anchor confined to another balloon: it goes to an anchor
//!   of its own balloon, else to an unconfined anchor (a `text_free` box, a
//!   box between balloons), else to no anchor. The same holds without pixels:
//!   anchors confined to two balloons never fold into one group however much
//!   their boxes overlap, and an ink estimate never leaves its group's
//!   balloon. A component that reaches out of its balloon, and a group whose
//!   lettering (its estimate included) lies in two balloons, are flagged.
//! - **Confidence**: an Ogkalu text box under [`TEXT_BOX_SCORE`] with no
//!   mask lettering under it, and a small CTD block its detector was unsure
//!   of (the box path's speckle rule, [`crate::detect::size_verdict`]), are
//!   candidates, not cleaning jobs.
//! - **Layout**: components no anchor claims are grouped by text layout
//!   (column and row adjacency, bounded by glyph size and
//!   [`LAYOUT_GAP_MAX`]), never across a balloon. Each such group is cleaned,
//!   recorded as unclaimed when a box detector ran and as isolated when it is
//!   a lone island without one.
//! - **Reach**: groups whose lettering comes within one larger glyph, at most
//!   96 px ([`reach`]), of each other, in the same balloon or both in none,
//!   are one group ([`within_reach`]), so an effect the boxes cut in
//!   pieces is one job and one crop. Only the grouping widens: the lettering
//!   each group cleans stays the mask's own pixels, and the fit adds its halo
//!   as before.
//!
//! ## Where it is used
//!
//! `src-tauri/src/run.rs` (production detection, every model selection, local
//! and cloud alike) and `crate::fusion` (the analysis preview) both call [`group`]; nothing
//! else decides what a cleaning unit is.
use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::balloon::{BalloonBox, BalloonClass};
use crate::detect::{DetBox, DetectedLanguage, Region, SizeVerdict};
use crate::mask::{Mask, Rect};

/// Bumped whenever a rule below changes which pixels land in which group, or
/// how a group's id is derived. Records keep the version they were made
/// under; an id is a stored string, so older groups load unchanged and only a
/// new detection names its groups by the current recipe.
///
/// 4: ids digest each component's exact pixels; pixel-less anchors confined
/// to two balloons never fold together; an estimate stays in its balloon.
/// 5: the mask decides what is lettering: a layout group is a cleaning job,
/// recorded when no box claimed it, and lettering within reach of other
/// lettering in one balloon (or both in none) is one group ([`within_reach`]).
pub const GROUPING_VERSION: u32 = 5;
/// Bubble boxes below this score are not walls. The balloon module's own
/// "sure" line, for the same reason: a weak shape is a statement about
/// something near the text, not a boundary.
pub const BUBBLE_SCORE: f32 = 0.5;
/// An Ogkalu text box below this score, with no lettering mask under it, is a
/// candidate rather than a cleaning job: the same "sure" line, and the one
/// the box path adopted uncovered text boxes at.
pub const TEXT_BOX_SCORE: f32 = 0.5;
/// Components no box claims and smaller than this stay evidence only.
pub const SPECK_PIXELS: u32 = 12;
/// Default split limit: the cloud analysis tile side and FLUX's working long
/// side. A group larger than this in either dimension is split.
pub const MAX_GROUP_SIDE: u32 = 1024;
/// Upper bound on any layout gap, in pixels: "bounded proximity".
pub const LAYOUT_GAP_MAX: i64 = 48;
/// Two anchors overlapping by more than this share of the smaller are one.
const DUPLICATE_SHARE: f64 = 0.5;
/// Two lobes of one balloon overlap by at least this share of the smaller.
const LOBE_SHARE: f64 = 0.3;
/// A component belongs to the anchor holding at least half its pixels.
const ASSIGN_SHARE: f64 = 0.5;
/// This share of a component outside its balloon flags the group.
const CROSSING_SHARE: f64 = 0.15;
/// This share of a component inside a second balloon's anchor flags it.
const BRIDGE_SHARE: f64 = 0.2;
/// Pixels either side of a model seam that still count as touching it.
const SEAM_SLACK: i64 = 3;
/// Glyph gap as a fraction of glyph size for layout adjacency.
const LAYOUT_GAP_FRACTION: f64 = 0.6;
/// Reach between two groups' lettering, as a fraction of the larger glyph,
/// and its bound in pixels ([`reach`]). Chosen on the five hand-marked pages
/// (1.0 joins the one text box 0.6 left in two, with no box joined to another
/// at any setting) and by eye on chapter 109: 96 px keeps "치지지직" whole
/// where 48 px split it, and keeps two effects apart that 128 px joined.
const REACH_FRACTION: f64 = 1.0;
const REACH_MAX: i64 = 96;

/* ------------------------------------------------------------------ */
/* Evidence types                                                      */
/* ------------------------------------------------------------------ */

/// The SAM-TS-L checkpoint this application exports its lettering-mask graphs
/// from, and the revision it pins (`spikes/sam-ts-l/PROVENANCE.md`,
/// `sam-bootstrap/bootstrap.py`, `deploy/cloud/common/analysis_seed.py`). A
/// separately published checkpoint: not current Koharu's layout detector
/// (`mayocream/koharu-layout-rfdetr-seg-2xl-1152`) and not Manga Text
/// Segmentation 2025 (`mayocream/manga-text-segmentation-2025`), neither of
/// which this application runs.
pub const SAM_TS_L_REPOSITORY: &str = "mayocream/koharu-text-sam-ts-l";
pub const SAM_TS_L_REVISION: &str = "5dd97423e0fbf2404264979136d47e8101144046";
/// The repository both Ogkalu detector sizes come from, and the revision the
/// Full graph is pinned to (`weights.rs`, `analysis_seed.py`).
pub const OGKALU_REPOSITORY: &str = "ogkalu/comic-text-and-bubble-detector";
pub const OGKALU_FULL_REVISION: &str = "16e8a622f91fabc6b5b65c96d32d1183f8843546";

/// A model whose output is evidence here. The names are the product names;
/// persisted model ids elsewhere (`samTs`, `ctd`, `rtFull`, `rtSmall`) are
/// [`EvidenceModel::setting_id`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceModel {
    /// `mayocream/koharu-text-sam-ts-l`: binary lettering segmentation.
    SamTsL,
    /// Comic Text Detector boxes.
    Ctd,
    /// `ogkalu/comic-text-and-bubble-detector`, full FP32 RT-DETR v2.
    OgkaluFull,
    /// The same repository's small INT8 variant.
    OgkaluSmall,
}

impl EvidenceModel {
    pub fn setting_id(self) -> &'static str {
        match self {
            EvidenceModel::SamTsL => "samTs",
            EvidenceModel::Ctd => "ctd",
            EvidenceModel::OgkaluFull => "rtFull",
            EvidenceModel::OgkaluSmall => "rtSmall",
        }
    }

    /// The product name, as the model choices read it.
    pub fn name(self) -> &'static str {
        match self {
            EvidenceModel::SamTsL => "SAM-TS-L lettering mask",
            EvidenceModel::Ctd => "Comic Text Detector (CTD)",
            EvidenceModel::OgkaluFull => "Ogkalu comic text & bubble detector - Full",
            EvidenceModel::OgkaluSmall => "Ogkalu comic text & bubble detector - Small",
        }
    }

    /// The published weights that ran: repository, and the file where the
    /// repository holds several. Diagnostics name this beside the product
    /// name, so SAM-TS-L is always named by its own checkpoint.
    pub fn checkpoint(self) -> &'static str {
        match self {
            EvidenceModel::SamTsL => SAM_TS_L_REPOSITORY,
            EvidenceModel::Ctd => "zyddnys/manga-image-translator comictextdetector.pt.onnx",
            EvidenceModel::OgkaluFull => "ogkalu/comic-text-and-bubble-detector detector.onnx (RT-DETR v2, FP32)",
            EvidenceModel::OgkaluSmall => "ogkalu/comic-text-and-bubble-detector detector-v4-s_int8.onnx (INT8)",
        }
    }

    /// The revision this build pins the weights to, where it pins one. The
    /// small Ogkalu graph is fetched from `main` and held to its SHA-256
    /// instead; CTD is a release asset.
    pub fn revision(self) -> Option<&'static str> {
        match self {
            EvidenceModel::SamTsL => Some(SAM_TS_L_REVISION),
            EvidenceModel::Ctd => Some("release beta-0.2.1"),
            EvidenceModel::OgkaluFull => Some(OGKALU_FULL_REVISION),
            EvidenceModel::OgkaluSmall => None,
        }
    }

    /// The preprocessing the model's input went through, for diagnostics.
    pub fn preprocessing(self) -> &'static str {
        match self {
            EvidenceModel::SamTsL => "RGB, longest side resized to 1024 (bilinear), top-left on a gray-128 1024 square; logits > 0; nearest restore to source size",
            EvidenceModel::Ctd => "1024 letterbox; YOLO boxes, segmentation head thresholded at 0.3",
            EvidenceModel::OgkaluFull | EvidenceModel::OgkaluSmall => "each input tile resized to 640 square (bilinear); score floor 0.35",
        }
    }
}

/// Where a model ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Execution {
    Local,
    Cloud,
}

impl Execution {
    pub fn label(self) -> &'static str {
        match self {
            Execution::Local => "local",
            Execution::Cloud => "cloud",
        }
    }
}

/// The spatial input a model saw. Tiling is part of the model input and is
/// recorded so a seam in the evidence is explainable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpatialInput {
    /// The whole page or segment crop in one inference.
    Whole,
    /// Two contiguous horizontal halves (local full RT-DETR).
    Halves,
    /// Non-overlapping page tiles of at most 1024 square: cloud analysis
    /// protocol 1.0.0. Read from older records only.
    CloudTiles,
    /// Overlapping 1024 tiles, each pixel and box taken from the tile whose
    /// core owns it ([`crate::cloud_tiles`]): cloud analysis protocol 1.1.0.
    OverlappingCloudTiles,
}

impl SpatialInput {
    /// The spatial input spelled out, cloud tile plans by protocol version.
    pub fn label(self) -> &'static str {
        match self {
            SpatialInput::Whole => "whole page (or one long-strip segment crop) in one inference",
            SpatialInput::Halves => "two horizontal halves",
            SpatialInput::CloudTiles => "cloud tiles 1.0.0: non-overlapping tiles of at most 1024",
            SpatialInput::OverlappingCloudTiles => {
                "cloud tiles 1.1.0: 1024 tiles overlapping by at least 256, stitched by core ownership"
            }
        }
    }
}

/// One model's contribution to a grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUse {
    pub model: EvidenceModel,
    pub execution: Execution,
    pub spatial: SpatialInput,
}

impl ModelUse {
    pub fn new(model: EvidenceModel, execution: Execution, spatial: SpatialInput) -> ModelUse {
        ModelUse { model, execution, spatial }
    }

    /// `name, checkpoint @ revision [execution, spatial input]`: which weights
    /// ran, where, on what. The line provenance and logs carry.
    pub fn identity(&self) -> String {
        let revision = self.model.revision().unwrap_or("revision not pinned");
        format!(
            "{}, {} @ {revision} [{}, {}]",
            self.model.name(),
            self.model.checkpoint(),
            self.execution.label(),
            self.spatial.label()
        )
    }

    /// [`ModelUse::identity`] and the model's own preprocessing, one line for
    /// a log or report.
    pub fn describe(&self) -> String {
        format!("{}: {}", self.identity(), self.model.preprocessing())
    }

    /// Every part of [`ModelUse::describe`] as a field, for JSON diagnostics.
    pub fn description(&self) -> ModelDescription {
        ModelDescription {
            model: self.model,
            name: self.model.name(),
            checkpoint: self.model.checkpoint(),
            revision: self.model.revision(),
            execution: self.execution.label(),
            spatial: self.spatial.label(),
            preprocessing: self.model.preprocessing(),
            described: self.describe(),
        }
    }
}

/// A [`ModelUse`] spelled out for diagnostic output (reports, the analysis
/// preview, a run's settings record). Written, never read back: the persisted
/// form of a model use is [`ModelUse`] itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescription {
    pub model: EvidenceModel,
    pub name: &'static str,
    pub checkpoint: &'static str,
    pub revision: Option<&'static str>,
    pub execution: &'static str,
    pub spatial: &'static str,
    pub preprocessing: &'static str,
    pub described: String,
}

/// [`ModelUse::description`] of each of `uses`.
pub fn describe_models(uses: &[ModelUse]) -> Vec<ModelDescription> {
    uses.iter().map(ModelUse::description).collect()
}

/// What a detector box says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoxKind {
    /// Ogkalu `bubble`: a wall, never a grouping anchor.
    Bubble,
    /// Ogkalu `text_bubble`.
    TextBubble,
    /// Ogkalu `text_free`.
    TextFree,
    /// A CTD text box.
    CtdText,
}

/// A raw detector box, as the model returned it (clipped to the raster).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectorBox {
    pub id: String,
    pub model: EvidenceModel,
    pub kind: BoxKind,
    pub rect: Rect,
    pub score: f32,
}

/// How a component reached its group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Assignment {
    /// At least half its pixels lie in the group's anchor boxes.
    TextBox,
    /// Punctuation or a stroke just outside the anchor, or another piece of
    /// the same text within reach of the group, in the same balloon.
    Proximity,
    /// Grouped by text layout with other unclaimed components.
    Layout,
}

/// Why a group or component needs a person to look at it. Each variant is a
/// statement about evidence, never a score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewReason {
    /// Lettering no text box claims, while a box detector ran.
    UnassignedComponent,
    /// SAM-only: a single island with no neighbour in text layout.
    IsolatedComponent,
    /// A component reaches out of its balloon, or into a second balloon's text.
    CrossesBalloon,
    /// A text box with no lettering pixels under it.
    MaskMissingUnderTextBox,
}

impl ReviewReason {
    /// The `review.reason.*` key a layer row shows.
    pub fn key(self) -> &'static str {
        match self {
            ReviewReason::UnassignedComponent => "review.reason.unassignedMask",
            ReviewReason::IsolatedComponent => "review.reason.isolatedMask",
            ReviewReason::CrossesBalloon => "review.reason.crossesBalloon",
            ReviewReason::MaskMissingUnderTextBox => "review.reason.maskMissingUnderBox",
        }
    }
}

/// One 8-connected island of the lettering mask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelComponent {
    /// `sam-NNNNN`, the 1-based label in row-major scan order.
    pub id: String,
    pub bounds: Rect,
    pub pixels: u32,
    /// The group this component belongs to. `None` for a speck no box claimed,
    /// which stays evidence only.
    pub group: Option<String>,
    pub via: Option<Assignment>,
    pub reasons: Vec<ReviewReason>,
}

/// What anchored a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GroupOrigin {
    /// At least one Ogkalu text box.
    TextBox,
    /// CTD boxes only.
    CtdBox,
    /// No box: text layout of unclaimed components.
    MaskLayout,
}

/// Whether a group is cleaned or listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Disposition {
    /// A cleaning job and a layer.
    Clean,
    /// Listed with its reason and left untouched.
    Candidate,
}

/// One part of a group split for a model limit. The part's id is the
/// parent's with `-p{index}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitPart {
    pub index: u32,
    pub of: u32,
    /// The id of the whole group the part was cut from. Empty on records
    /// written before parts named their parent.
    #[serde(default)]
    pub parent: String,
}

/// A logical text group: the cleaning unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextGroup {
    /// Exact-evidence identity, `tg-` + 12 hex (see the module notes); a
    /// split part's is its parent's with `-p{k}`.
    pub id: String,
    /// Reading order among this grouping's groups.
    pub order: u32,
    pub origin: GroupOrigin,
    pub disposition: Disposition,
    /// The lettering's bounds; the anchor's for a group with no lettering.
    pub bounds: Rect,
    /// Hull of the anchoring boxes, if any.
    pub anchor: Option<Rect>,
    /// The balloon (bubble box id) the group is confined to.
    pub bubble: Option<String>,
    pub bubble_rect: Option<Rect>,
    pub component_ids: Vec<String>,
    pub box_ids: Vec<String>,
    pub models: Vec<EvidenceModel>,
    pub lettering_pixels: u32,
    /// Strongest contributing box score; `None` for a layout group.
    pub score: Option<f32>,
    pub reasons: Vec<ReviewReason>,
    pub split: Option<SplitPart>,
    /// The lettering includes a bounded ink estimate under the group's text
    /// boxes, beside any model pixels: the mask had (almost) nothing there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
    /// 1-based component labels, for [`Grouping::lettering`].
    #[serde(skip)]
    labels: Vec<u32>,
    /// The estimated pixels, raster coordinates, when `estimated`.
    #[serde(skip)]
    estimate: Option<Mask>,
}

/// A tile boundary of an upstream model input, in this raster's coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seam {
    /// The line `x = n`.
    Vertical(i64),
    /// The line `y = n`.
    Horizontal(i64),
}

/// Everything [`group`] reads.
pub struct Inputs<'a> {
    pub width: u32,
    pub height: u32,
    /// This raster's origin on its page. Group ids hash page coordinates, so a
    /// group detected in two overlapping segment crops keeps one identity.
    pub origin: (i64, i64),
    /// The lettering mask, one byte per pixel; nonzero is lettering.
    pub mask: Option<&'a [u8]>,
    /// The model whose pixels `mask` holds: SAM-TS-L, or CTD's segmentation
    /// where SAM was not selected.
    pub pixel_model: EvidenceModel,
    /// The raster's pixels, for the bounded ink estimate under a text box no
    /// mask pixel covers. `None` leaves such a box without lettering.
    pub page: Option<&'a crate::image::Raster>,
    /// Ogkalu boxes, every class.
    pub rt: &'a [BalloonBox],
    pub ctd: &'a [DetBox],
    /// Every model that ran, including ones that returned nothing: a box
    /// detector that ran and found no box is what makes unclaimed lettering a
    /// candidate rather than SAM-only text.
    pub models: Vec<ModelUse>,
    pub seams: Vec<Seam>,
    pub max_side: u32,
    /// What page this raster is of (the source's SHA-256 and the page), so
    /// group ids from different pages never collide. Empty when unknown.
    pub scope: String,
}

impl<'a> Inputs<'a> {
    /// Mask-only inputs with defaults, for callers that add boxes after.
    pub fn new(width: u32, height: u32, mask: Option<&'a [u8]>) -> Inputs<'a> {
        Inputs {
            width,
            height,
            origin: (0, 0),
            mask,
            pixel_model: EvidenceModel::SamTsL,
            page: None,
            rt: &[],
            ctd: &[],
            models: Vec::new(),
            seams: Vec::new(),
            max_side: MAX_GROUP_SIDE,
            scope: String::new(),
        }
    }

    fn box_detector_ran(&self) -> bool {
        self.models.iter().any(|use_| use_.model != EvidenceModel::SamTsL)
            || !self.rt.is_empty()
            || !self.ctd.is_empty()
    }

    fn rt_model(&self) -> EvidenceModel {
        self.models
            .iter()
            .map(|use_| use_.model)
            .find(|model| matches!(model, EvidenceModel::OgkaluFull | EvidenceModel::OgkaluSmall))
            .unwrap_or(EvidenceModel::OgkaluFull)
    }
}

/// The grouping of one raster: evidence plus groups.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grouping {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub origin: (i64, i64),
    /// Whose pixels the components are ([`Inputs::pixel_model`]).
    #[serde(default = "sam_pixels")]
    pub pixel_model: EvidenceModel,
    pub models: Vec<ModelUse>,
    pub components: Vec<PixelComponent>,
    pub boxes: Vec<DetectorBox>,
    pub groups: Vec<TextGroup>,
    #[serde(skip)]
    labels: Vec<u32>,
}

fn sam_pixels() -> EvidenceModel {
    EvidenceModel::SamTsL
}

/* ------------------------------------------------------------------ */
/* Labeling                                                            */
/* ------------------------------------------------------------------ */

/// A labeled mask: `labels[i]` is 0 for background, else a 1-based label.
#[derive(Debug, Clone)]
pub struct Labeled {
    pub labels: Vec<u32>,
    /// `(bounds, pixels)` per label, index `label - 1`.
    pub components: Vec<(Rect, u32)>,
}

/// 8-connected components in row-major scan order. The ids this produces
/// (`sam-{label:05}`) are the analysis view's component ids too.
pub fn label(width: u32, height: u32, mask: &[u8]) -> Labeled {
    let (w, h) = (width as usize, height as usize);
    let mut labels = vec![0u32; w * h];
    let mut components = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if mask[start] == 0 || labels[start] != 0 {
            continue;
        }
        let number = components.len() as u32 + 1;
        labels[start] = number;
        stack.push(start);
        let (mut left, mut top, mut right, mut bottom) = (w, h, 0usize, 0usize);
        let mut pixels = 0u32;
        while let Some(at) = stack.pop() {
            let (x, y) = (at % w, at / w);
            pixels += 1;
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let next = ny * w + nx;
                    if mask[next] != 0 && labels[next] == 0 {
                        labels[next] = number;
                        stack.push(next);
                    }
                }
            }
        }
        components.push((
            Rect::new(left as i64, top as i64, (right - left) as u32, (bottom - top) as u32),
            pixels,
        ));
    }
    Labeled { labels, components }
}

pub fn component_id(label: u32) -> String {
    format!("sam-{label:05}")
}

/// A component id for `model`'s pixels: `sam-NNNNN` for SAM-TS-L, whose ids
/// the analysis view already uses, and `seg-NNNNN` for CTD's segmentation.
pub fn component_id_of(model: EvidenceModel, label: u32) -> String {
    match model {
        EvidenceModel::Ctd => format!("seg-{label:05}"),
        _ => component_id(label),
    }
}

/// The 1-based label a component id names.
pub(crate) fn component_label(id: &str) -> Option<u32> {
    id.strip_prefix("sam-").or_else(|| id.strip_prefix("seg-")).and_then(|n| n.parse().ok())
}

/* ------------------------------------------------------------------ */
/* Geometry                                                            */
/* ------------------------------------------------------------------ */

fn area(rect: &Rect) -> i64 {
    rect.w as i64 * rect.h as i64
}

fn intersection(a: &Rect, b: &Rect) -> i64 {
    let w = (a.right().min(b.right()) - a.x.max(b.x)).max(0);
    let h = (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0);
    w * h
}

fn hull(a: &Rect, b: &Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect::new(x, y, (a.right().max(b.right()) - x) as u32, (a.bottom().max(b.bottom()) - y) as u32)
}

/// Horizontal and vertical separation; zero where the extents overlap.
fn gaps(a: &Rect, b: &Rect) -> (i64, i64) {
    let gx = (b.x - a.right()).max(a.x - b.right()).max(0);
    let gy = (b.y - a.bottom()).max(a.y - b.bottom()).max(0);
    (gx, gy)
}

fn overlap_x(a: &Rect, b: &Rect) -> i64 {
    a.right().min(b.right()) - a.x.max(b.x)
}

fn overlap_y(a: &Rect, b: &Rect) -> i64 {
    a.bottom().min(b.bottom()) - a.y.max(b.y)
}

fn centre(rect: &Rect) -> (i64, i64) {
    (rect.x + rect.w as i64 / 2, rect.y + rect.h as i64 / 2)
}

fn grown_by(rect: &Rect, by: i64, width: u32, height: u32) -> Rect {
    let x = (rect.x - by).max(0);
    let y = (rect.y - by).max(0);
    let right = (rect.right() + by).min(width as i64);
    let bottom = (rect.bottom() + by).min(height as i64);
    Rect::new(x, y, (right - x).max(0) as u32, (bottom - y).max(0) as u32)
}

fn clipped(rect: Rect, width: u32, height: u32) -> Rect {
    let x = rect.x.clamp(0, width as i64);
    let y = rect.y.clamp(0, height as i64);
    let right = rect.right().clamp(0, width as i64);
    let bottom = rect.bottom().clamp(0, height as i64);
    Rect::new(x, y, (right - x).max(0) as u32, (bottom - y).max(0) as u32)
}

fn in_reading_order(a: &Rect, b: &Rect) -> std::cmp::Ordering {
    a.y.cmp(&b.y).then(a.x.cmp(&b.x)).then(a.h.cmp(&b.h)).then(a.w.cmp(&b.w))
}

/// Whether two rectangles were cut apart by one of `seams`: one ends at the
/// seam, the other starts there, and they share at least half the smaller
/// extent along it.
fn split_by_seam(a: &Rect, b: &Rect, seams: &[Seam]) -> bool {
    let near = |edge: i64, line: i64| (edge - line).abs() <= SEAM_SLACK;
    seams.iter().any(|seam| match *seam {
        Seam::Horizontal(y) => {
            let touching = (near(a.bottom(), y) && near(b.y, y)) || (near(b.bottom(), y) && near(a.y, y));
            touching && overlap_x(a, b) * 2 >= a.w.min(b.w) as i64
        }
        Seam::Vertical(x) => {
            let touching = (near(a.right(), x) && near(b.x, x)) || (near(b.right(), x) && near(a.x, x));
            touching && overlap_y(a, b) * 2 >= a.h.min(b.h) as i64
        }
    })
}

/// Union-find whose root is always the lowest index, so the partition does
/// not depend on the order unions happen in.
struct Partition(Vec<usize>);

impl Partition {
    fn new(n: usize) -> Partition {
        Partition((0..n).collect())
    }

    fn root(&mut self, i: usize) -> usize {
        let mut at = i;
        while self.0[at] != at {
            self.0[at] = self.0[self.0[at]];
            at = self.0[at];
        }
        at
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.root(a), self.root(b));
        if ra != rb {
            let (low, high) = (ra.min(rb), ra.max(rb));
            self.0[high] = low;
        }
    }

    fn classes(&mut self) -> Vec<Vec<usize>> {
        let n = self.0.len();
        let mut at = vec![usize::MAX; n];
        let mut out: Vec<Vec<usize>> = Vec::new();
        for i in 0..n {
            let root = self.root(i);
            if at[root] == usize::MAX {
                at[root] = out.len();
                out.push(Vec::new());
            }
            out[at[root]].push(i);
        }
        out
    }
}

/* ------------------------------------------------------------------ */
/* Grouping                                                            */
/* ------------------------------------------------------------------ */

/// An anchor box, already confined to a balloon.
struct Anchor {
    box_index: usize,
    rect: Rect,
    bubble: Option<usize>,
}

/// A group under construction.
struct Draft {
    origin: GroupOrigin,
    disposition: Disposition,
    anchors: Vec<Rect>,
    bubble: Option<usize>,
    box_indices: Vec<usize>,
    /// 0-based component indices.
    members: Vec<usize>,
    reasons: BTreeSet<ReviewReason>,
    score: Option<f32>,
    /// Raster pixels of the bounded ink estimate, row-major, sorted.
    ink: Vec<usize>,
}

impl Draft {
    fn lettering_bounds(&self, components: &[(Rect, u32)]) -> Option<Rect> {
        self.members.iter().map(|&m| components[m].0).reduce(|a, b| hull(&a, &b))
    }

    /// The median glyph extent of this group's non-speck members.
    fn glyph(&self, components: &[(Rect, u32)]) -> i64 {
        let mut sizes: Vec<i64> = self
            .members
            .iter()
            .filter(|&&m| components[m].1 >= SPECK_PIXELS)
            .map(|&m| components[m].0.w.max(components[m].0.h) as i64)
            .collect();
        if sizes.is_empty() {
            sizes = self.members.iter().map(|&m| components[m].0.w.max(components[m].0.h) as i64).collect();
        }
        if sizes.is_empty() {
            return self.anchors.iter().map(|r| r.w.min(r.h) as i64).min().unwrap_or(16);
        }
        sizes.sort_unstable();
        sizes[sizes.len() / 2]
    }
}

/// Group one raster's evidence. See the module notes for the rules.
pub fn group(inputs: &Inputs) -> Result<Grouping, String> {
    let (width, height) = (inputs.width, inputs.height);
    let length = width as usize * height as usize;
    if width == 0 || height == 0 || length > 100_000_000 {
        return Err("grouping dimensions are invalid or too large".into());
    }
    if inputs.mask.is_some_and(|mask| mask.len() != length) {
        return Err("lettering mask dimensions are invalid".into());
    }
    if inputs.page.is_some_and(|page| (page.width, page.height) != (width, height)) {
        return Err("grouping page dimensions differ from its evidence".into());
    }
    let labeled = inputs.mask.map(|mask| label(width, height, mask));
    let components: Vec<(Rect, u32)> = labeled.as_ref().map(|l| l.components.clone()).unwrap_or_default();
    let labels: Vec<u32> = labeled.map(|l| l.labels).unwrap_or_default();

    // Raw boxes, in input order: the ids name input positions.
    let rt_model = inputs.rt_model();
    let mut boxes: Vec<DetectorBox> = Vec::new();
    for (i, item) in inputs.rt.iter().enumerate() {
        boxes.push(DetectorBox {
            id: format!("rt-{i:04}"),
            model: rt_model,
            kind: match item.class {
                BalloonClass::Bubble => BoxKind::Bubble,
                BalloonClass::TextInBubble => BoxKind::TextBubble,
                BalloonClass::TextFree => BoxKind::TextFree,
            },
            rect: clipped(item.rect, width, height),
            score: item.score,
        });
    }
    for (i, item) in inputs.ctd.iter().enumerate() {
        boxes.push(DetectorBox {
            id: format!("ctd-{i:04}"),
            model: EvidenceModel::Ctd,
            kind: BoxKind::CtdText,
            rect: clipped(item.rect, width, height),
            score: item.confidence,
        });
    }
    // Processing order is geometric, never input order.
    let mut order: Vec<usize> = (0..boxes.len()).filter(|&i| area(&boxes[i].rect) > 0).collect();
    order.sort_by(|&a, &b| {
        in_reading_order(&boxes[a].rect, &boxes[b].rect)
            .then(boxes[a].kind.cmp(&boxes[b].kind))
            .then(boxes[b].score.total_cmp(&boxes[a].score))
            .then(boxes[a].model.cmp(&boxes[b].model))
    });

    // Balloons: sure bubble boxes, with lobes and seam-cut halves joined.
    let bubbles: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| boxes[i].kind == BoxKind::Bubble && boxes[i].score >= BUBBLE_SCORE)
        .collect();
    let mut walls = Partition::new(bubbles.len());
    for a in 0..bubbles.len() {
        for b in a + 1..bubbles.len() {
            let (ra, rb) = (boxes[bubbles[a]].rect, boxes[bubbles[b]].rect);
            let smaller = area(&ra).min(area(&rb));
            if intersection(&ra, &rb) as f64 >= LOBE_SHARE * smaller as f64
                || split_by_seam(&ra, &rb, &inputs.seams)
            {
                walls.union(a, b);
            }
        }
    }
    let wall_classes = walls.classes();
    let mut wall_of = vec![0usize; bubbles.len()];
    for (class, members) in wall_classes.iter().enumerate() {
        for &m in members {
            wall_of[m] = class;
        }
    }
    let wall_rects: Vec<Rect> = wall_classes
        .iter()
        .map(|members| members.iter().map(|&m| boxes[bubbles[m]].rect).reduce(|a, b| hull(&a, &b)).unwrap())
        .collect();
    let wall_ids: Vec<String> = wall_classes.iter().map(|members| boxes[bubbles[members[0]]].id.clone()).collect();
    // The balloon a point is in: the smallest sure bubble box holding it.
    let balloon_at = |x: i64, y: i64| -> Option<usize> {
        bubbles
            .iter()
            .enumerate()
            .filter(|(_, &b)| boxes[b].rect.contains(x, y))
            .min_by_key(|(k, &b)| (area(&boxes[b].rect), *k))
            .map(|(k, _)| wall_of[k])
    };

    // Anchors: Ogkalu text boxes, then CTD boxes.
    let anchors: Vec<Anchor> = order
        .iter()
        .copied()
        .filter(|&i| matches!(boxes[i].kind, BoxKind::TextBubble | BoxKind::TextFree | BoxKind::CtdText))
        .map(|i| {
            let rect = boxes[i].rect;
            let (cx, cy) = centre(&rect);
            // `text_free` is a statement about this text: it is outside.
            let bubble = if boxes[i].kind == BoxKind::TextFree { None } else { balloon_at(cx, cy) };
            Anchor { box_index: i, rect, bubble }
        })
        .collect();
    let mut joined = Partition::new(anchors.len());
    for a in 0..anchors.len() {
        for b in a + 1..anchors.len() {
            let (x, y) = (&anchors[a], &anchors[b]);
            if x.bubble != y.bubble {
                continue;
            }
            let smaller = area(&x.rect).min(area(&y.rect));
            let duplicate = intersection(&x.rect, &y.rect) as f64 > DUPLICATE_SHARE * smaller as f64;
            let seam = split_by_seam(&x.rect, &y.rect, &inputs.seams);
            let (gx, gy) = gaps(&x.rect, &y.rect);
            let short = x.rect.w.min(x.rect.h).min(y.rect.w.min(y.rect.h)) as i64;
            let near = x.bubble.is_some() && gx.max(gy) <= (short / 2).max(12);
            if duplicate || seam || near {
                joined.union(a, b);
            }
        }
    }
    let mut drafts: Vec<Draft> = joined
        .classes()
        .into_iter()
        .map(|members| {
            let box_indices: Vec<usize> = members.iter().map(|&m| anchors[m].box_index).collect();
            let ogkalu = box_indices.iter().any(|&i| boxes[i].kind != BoxKind::CtdText);
            Draft {
                origin: if ogkalu { GroupOrigin::TextBox } else { GroupOrigin::CtdBox },
                disposition: Disposition::Clean,
                anchors: members.iter().map(|&m| anchors[m].rect).collect(),
                bubble: anchors[members[0]].bubble,
                score: box_indices.iter().map(|&i| boxes[i].score).reduce(f32::max),
                box_indices,
                members: Vec::new(),
                reasons: BTreeSet::new(),
                ink: Vec::new(),
            }
        })
        .collect();

    // The balloon each component's centre is in.
    let here_of: Vec<Option<usize>> = components
        .iter()
        .map(|(bounds, _)| {
            let (cx, cy) = centre(bounds);
            balloon_at(cx, cy)
        })
        .collect();

    // Assignment by majority of pixels inside an anchor. A glyph in a balloon
    // goes to an anchor of that balloon when one is near; with none near, an
    // unconfined anchor (a `text_free` box, a box between balloons) may take
    // it. An anchor confined to another balloon never does, however much of
    // the glyph its box covers. A glyph in no balloon may go to any anchor,
    // and one reaching out of the anchor's balloon is flagged below.
    let margins: Vec<Vec<Rect>> = drafts
        .iter()
        .map(|draft| {
            draft
                .anchors
                .iter()
                .map(|r| grown_by(r, (r.w.min(r.h) as i64 / 10).clamp(2, 12), width, height))
                .collect()
        })
        .collect();
    let mut owner: Vec<Option<(usize, Assignment)>> = vec![None; components.len()];
    let mut component_reasons: Vec<BTreeSet<ReviewReason>> = vec![BTreeSet::new(); components.len()];
    // Per draft: the drafts that won components it also held at least half of.
    let mut outbid: Vec<Vec<(usize, u64)>> = vec![Vec::new(); drafts.len()];
    for (index, (bounds, pixels)) in components.iter().enumerate() {
        let near: Vec<usize> = (0..drafts.len())
            .filter(|&d| margins[d].iter().any(|r| intersection(r, bounds) > 0))
            .collect();
        if near.is_empty() {
            continue;
        }
        let here = here_of[index];
        let own_balloon = here.is_some() && near.iter().any(|&d| drafts[d].bubble == here);
        let eligible =
            |d: usize| here.is_none() || drafts[d].bubble == here || (!own_balloon && drafts[d].bubble.is_none());
        let number = index as u32 + 1;
        let mut shared = vec![0u32; near.len()];
        let mut outside_bubble = vec![0u32; near.len()];
        for y in bounds.y..bounds.bottom() {
            for x in bounds.x..bounds.right() {
                if labels[y as usize * width as usize + x as usize] != number {
                    continue;
                }
                for (k, &d) in near.iter().enumerate() {
                    if margins[d].iter().any(|r| r.contains(x, y)) {
                        shared[k] += 1;
                    }
                    if let Some(wall) = drafts[d].bubble {
                        if !grown_by(&wall_rects[wall], 3, width, height).contains(x, y) {
                            outside_bubble[k] += 1;
                        }
                    }
                }
            }
        }
        let held = |k: usize| shared[k] as f64 >= ASSIGN_SHARE * *pixels as f64;
        let Some(best) = (0..near.len())
            .filter(|&k| eligible(near[k]))
            .max_by(|&a, &b| shared[a].cmp(&shared[b]).then(b.cmp(&a)))
            .filter(|&k| held(k))
        else {
            continue;
        };
        let d = near[best];
        owner[index] = Some((d, Assignment::TextBox));
        for (k, &other) in near.iter().enumerate() {
            if other != d && held(k) {
                outbid[other].push((d, u64::from(*pixels)));
            }
        }
        let crosses_out = drafts[d].bubble.is_some() && outside_bubble[best] as f64 >= CROSSING_SHARE * *pixels as f64;
        let bridges = near.iter().enumerate().any(|(k, &other)| {
            other != d && drafts[other].bubble != drafts[d].bubble && shared[k] as f64 >= BRIDGE_SHARE * *pixels as f64
        });
        if crosses_out || bridges {
            component_reasons[index].insert(ReviewReason::CrossesBalloon);
        }
    }
    for (index, slot) in owner.iter().enumerate() {
        if let Some((d, _)) = slot {
            drafts[*d].members.push(index);
        }
    }

    // Duplicate anchors. A text box with no lettering of its own is another
    // group's box as well, never a second job over the same glyphs and never a
    // text box with nothing under it, when
    // - lettering it held at least half of went to a group of its own balloon
    //   (a box confined to one balloon never folds into another's group), or
    // - it overlaps another anchor by more than [`DUPLICATE_SHARE`] of the
    //   smaller box, and the two are not confined to different balloons: a
    //   `text_free` and a `text_bubble` box over one caption are one caption,
    //   but two balloons' boxes are two jobs however much they overlap, as
    //   their glyphs would be. [`fold`] refuses a chain through an unconfined
    //   box that would join two balloons all the same.
    let mut into: Vec<Option<usize>> = vec![None; drafts.len()];
    for e in 0..drafts.len() {
        if !drafts[e].members.is_empty() {
            continue;
        }
        let mut won: Vec<(usize, u64)> = Vec::new();
        for &(winner, pixels) in &outbid[e] {
            if drafts[e].bubble.is_some() && drafts[winner].bubble != drafts[e].bubble {
                continue;
            }
            match won.iter_mut().find(|(w, _)| *w == winner) {
                Some(entry) => entry.1 += pixels,
                None => won.push((winner, pixels)),
            }
        }
        let by_lettering = won.iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|&(w, _)| w);
        let by_overlap = || {
            (0..drafts.len())
                .filter(|&t| t != e)
                .filter(|&t| drafts[e].bubble.is_none() || drafts[t].bubble.is_none() || drafts[t].bubble == drafts[e].bubble)
                .filter_map(|t| {
                    let share = drafts[e]
                        .anchors
                        .iter()
                        .flat_map(|a| drafts[t].anchors.iter().map(move |b| (a, b)))
                        .map(|(a, b)| intersection(a, b) as f64 / area(a).min(area(b)).max(1) as f64)
                        .fold(0.0, f64::max);
                    (share > DUPLICATE_SHARE).then_some((share, !drafts[t].members.is_empty(), t))
                })
                .max_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)))
                .map(|(_, _, t)| t)
        };
        into[e] = by_lettering.or_else(by_overlap);
    }
    let mut drafts = fold(drafts, &into, &mut owner, &boxes);

    // Proximity: punctuation and strokes just outside an anchor, same balloon.
    attach_by_proximity(&mut drafts, &mut owner, &components, &here_of);

    // Detector-only anchors: a text box with no lettering under it.
    if inputs.mask.is_some() {
        for draft in drafts.iter_mut().filter(|d| d.members.is_empty()) {
            draft.reasons.insert(ReviewReason::MaskMissingUnderTextBox);
        }
    }

    // Layout: unclaimed components (specks excluded) grouped by text layout.
    // Adjacency needs a vertical gap of at most [`LAYOUT_GAP_MAX`], so with
    // the components sorted by top edge each is compared only with those
    // starting within that reach of its bottom.
    let mut loose: Vec<usize> = (0..components.len())
        .filter(|&i| owner[i].is_none() && components[i].1 >= SPECK_PIXELS)
        .collect();
    loose.sort_by_key(|&i| (components[i].0.y, i));
    let mut layout = Partition::new(loose.len());
    for a in 0..loose.len() {
        let upper = components[loose[a]].0;
        for b in a + 1..loose.len() {
            let lower = components[loose[b]].0;
            if lower.y - upper.bottom() > LAYOUT_GAP_MAX {
                break;
            }
            if here_of[loose[a]] == here_of[loose[b]] && adjacent(&upper, &lower) {
                layout.union(a, b);
            }
        }
    }
    // The mask decides what is lettering, so each layout group is a cleaning
    // job. What no box claimed carries its reason, for the record.
    let box_ran = inputs.box_detector_ran();
    for class in layout.classes() {
        let mut members: Vec<usize> = class.iter().map(|&k| loose[k]).collect();
        members.sort_unstable();
        let mut reasons = BTreeSet::new();
        if box_ran {
            reasons.insert(ReviewReason::UnassignedComponent);
        } else if members.len() < 2 {
            reasons.insert(ReviewReason::IsolatedComponent);
        }
        let d = drafts.len();
        for &m in &members {
            owner[m] = Some((d, Assignment::Layout));
            for reason in &reasons {
                component_reasons[m].insert(*reason);
            }
        }
        drafts.push(Draft {
            origin: GroupOrigin::MaskLayout,
            disposition: Disposition::Clean,
            anchors: Vec::new(),
            bubble: here_of[members[0]],
            box_indices: Vec::new(),
            members,
            reasons,
            score: None,
            ink: Vec::new(),
        });
    }
    // Specks beside a layout group (an ellipsis, a dakuten) are its lettering
    // too, as they are beside an anchored group.
    attach_by_proximity(&mut drafts, &mut owner, &components, &here_of);

    // Crossing flags: a component's is its group's too, and a group whose
    // lettering lies in two balloons (an unconfined box took both) is flagged.
    for (index, slot) in owner.iter().enumerate() {
        if let Some((d, _)) = slot {
            if component_reasons[index].contains(&ReviewReason::CrossesBalloon) {
                drafts[*d].reasons.insert(ReviewReason::CrossesBalloon);
            }
        }
    }
    for draft in &mut drafts {
        let balloons: BTreeSet<usize> = draft.members.iter().filter_map(|&m| here_of[m]).collect();
        if balloons.len() > 1 {
            draft.reasons.insert(ReviewReason::CrossesBalloon);
        }
    }

    // Detections too weak to clean on their say alone are listed instead: a
    // small CTD block the detector was unsure of is speckle, which the box
    // path always dropped ([`crate::detect::size_verdict`]), and an Ogkalu
    // text box under [`TEXT_BOX_SCORE`] with no lettering under it is not
    // cleaned from an ink estimate.
    let ctd_median = crate::detect::median_rect_area(
        &boxes.iter().filter(|b| b.kind == BoxKind::CtdText).map(|b| b.rect).collect::<Vec<_>>(),
    );
    for draft in drafts.iter_mut().filter(|d| d.disposition == Disposition::Clean && !d.box_indices.is_empty()) {
        let ctd_only = draft.box_indices.iter().all(|&i| boxes[i].kind == BoxKind::CtdText);
        let hull_rect = draft.anchors.iter().copied().reduce(|a, b| hull(&a, &b)).unwrap_or(Rect::new(0, 0, 0, 0));
        let score = draft.score.unwrap_or(0.0);
        let speckle = ctd_only
            && crate::detect::size_verdict(area(&hull_rect), ctd_median, hull_rect.h, height, score) == SizeVerdict::Drop;
        let weak = draft.members.is_empty()
            && draft.box_indices.iter().all(|&i| boxes[i].kind != BoxKind::CtdText)
            && score < TEXT_BOX_SCORE;
        if !(speckle || weak) {
            continue;
        }
        draft.disposition = Disposition::Candidate;
        if draft.members.is_empty() {
            draft.reasons.insert(ReviewReason::MaskMissingUnderTextBox);
        } else {
            draft.reasons.insert(ReviewReason::IsolatedComponent);
            for &m in &draft.members {
                component_reasons[m].insert(ReviewReason::IsolatedComponent);
            }
        }
    }

    // Reach: one text the boxes cut apart is one job. Cleaning groups whose
    // lettering is within reach of each other, in the same balloon, are one
    // group, so an effect the detector boxed in two pieces, or whose strokes
    // no box claimed, is cleaned as one crop rather than piece by piece. It
    // runs on the settled dispositions, so a candidate never takes cleaning
    // lettering with it. The reach only decides the grouping: no pixel is
    // added to the lettering here. Lettering that joined a box's group is
    // claimed by it and loses its reason.
    let into = within_reach(&drafts, &owner, &components, &labels, &boxes, width, height);
    drafts = fold(drafts, &into, &mut owner, &boxes);
    for (index, slot) in owner.iter_mut().enumerate() {
        let Some((d, via)) = slot else { continue };
        if *via == Assignment::Layout && drafts[*d].origin != GroupOrigin::MaskLayout {
            *via = Assignment::Proximity;
            component_reasons[index].remove(&ReviewReason::UnassignedComponent);
            component_reasons[index].remove(&ReviewReason::IsolatedComponent);
        }
    }
    for draft in drafts.iter_mut().filter(|d| d.disposition == Disposition::Clean && d.origin != GroupOrigin::MaskLayout) {
        draft.reasons.remove(&ReviewReason::UnassignedComponent);
        draft.reasons.remove(&ReviewReason::IsolatedComponent);
    }

    // The bounded ink estimate, claimed in reading order. A group confined to
    // a balloon takes ink only inside it, as it takes glyphs: within three
    // pixels of its bubble box, the margin the crossing flag allows a glyph.
    if let Some(page) = inputs.page {
        let sequence = reading_sequence(&drafts, &components);
        let walls: Vec<Rect> = wall_rects.iter().map(|wall| grown_by(wall, 3, width, height)).collect();
        let into = estimate_ink(page, inputs.mask, &labels, &owner, &components, &boxes, &sequence, &walls, &mut drafts);
        drafts = fold(drafts, &into, &mut owner, &boxes);
    }

    // Pieces: the mask's components, then each estimate's 8-connected ink
    // islands. Naming and splitting treat both alike.
    // A group whose lettering, its estimate included, lies in two balloons is
    // flagged as its components would be: only an unconfined box takes both.
    let mut pieces: Vec<(Rect, u32)> = components.clone();
    let mut island_pixels: Vec<Vec<usize>> = Vec::new();
    let mut ink_members: Vec<Vec<usize>> = vec![Vec::new(); drafts.len()];
    let mut crossing = vec![false; drafts.len()];
    for (d, draft) in drafts.iter().enumerate() {
        let mut balloons: BTreeSet<usize> = draft.members.iter().filter_map(|&m| here_of[m]).collect();
        for island in ink_islands(&draft.ink, width) {
            let bounds = pixels_bounds(&island, width);
            let (cx, cy) = centre(&bounds);
            balloons.extend(balloon_at(cx, cy));
            ink_members[d].push(pieces.len());
            pieces.push((bounds, island.len() as u32));
            island_pixels.push(island);
        }
        crossing[d] = balloons.len() > 1;
    }
    for (draft, crosses) in drafts.iter_mut().zip(crossing) {
        if crosses {
            draft.reasons.insert(ReviewReason::CrossesBalloon);
        }
    }

    // Name each group once, split it for the model limit, and order.
    let mask_model: Vec<EvidenceModel> = inputs.mask.map(|_| inputs.pixel_model).into_iter().collect();
    let (ox, oy) = inputs.origin;
    let mut taken_ids: BTreeSet<String> = BTreeSet::new();
    let mut groups: Vec<TextGroup> = Vec::new();
    for d in reading_sequence(&drafts, &components) {
        let draft = &drafts[d];
        let mut members: Vec<usize> = draft.members.iter().chain(&ink_members[d]).copied().collect();
        members.sort_unstable();
        let anchor = draft.anchors.iter().copied().reduce(|a, b| hull(&a, &b));
        // The id digests exactly this evidence, in page coordinates, within
        // the page it was found on: each component's and ink island's own
        // pixels, not only its bounds and count, so two different glyph runs
        // of one size never share an id, and a re-detection of the same
        // pixels names the same group in any crop. A second group with
        // identical evidence takes the next free disambiguator, so no two ids
        // on a page collide.
        let mut lines: Vec<String> = members
            .iter()
            .map(|&m| {
                let (r, pixels) = pieces[m];
                let mut bits = vec![0u8; r.w as usize * r.h as usize];
                let local = |x: usize, y: usize| (y - r.y as usize) * r.w as usize + x - r.x as usize;
                let kind = if m < components.len() {
                    for y in r.y as usize..r.bottom() as usize {
                        for x in r.x as usize..r.right() as usize {
                            if labels[y * width as usize + x] == m as u32 + 1 {
                                bits[local(x, y)] = 1;
                            }
                        }
                    }
                    'c'
                } else {
                    for &at in &island_pixels[m - components.len()] {
                        bits[local(at % width as usize, at / width as usize)] = 1;
                    }
                    'i'
                };
                format!("{kind}{},{},{},{},{pixels},{:x};", r.x + ox, r.y + oy, r.w, r.h, Sha256::digest(&bits))
            })
            .collect();
        lines.sort_unstable();
        let mut evidence = format!("tg{GROUPING_VERSION}|{}|{:?}|", inputs.scope, draft.origin);
        for line in &lines {
            evidence.push_str(line);
        }
        if let Some(anchor) = anchor {
            evidence.push_str(&format!("a{},{},{},{};", anchor.x + ox, anchor.y + oy, anchor.w, anchor.h));
        }
        let id = (0u32..)
            .map(|k| {
                let salted = if k == 0 { evidence.clone() } else { format!("{evidence}#{k}") };
                format!("tg-{}", &format!("{:x}", Sha256::digest(salted.as_bytes()))[..12])
            })
            .find(|id| !taken_ids.contains(id))
            .unwrap();
        taken_ids.insert(id.clone());
        let mut box_ids: Vec<String> = draft.box_indices.iter().map(|&i| boxes[i].id.clone()).collect();
        box_ids.sort();
        let parts = split_members(&members, &pieces, inputs.max_side.max(1));
        let of = parts.len() as u32;
        for (k, part) in parts.into_iter().enumerate() {
            let (real, ink): (Vec<usize>, Vec<usize>) = part.iter().partition(|&&m| m < components.len());
            let bounds = part
                .iter()
                .map(|&m| pieces[m].0)
                .reduce(|a, b| hull(&a, &b))
                .or(anchor)
                .unwrap_or(Rect::new(0, 0, 0, 0));
            let mut models: BTreeSet<EvidenceModel> = draft.box_indices.iter().map(|&i| boxes[i].model).collect();
            if !real.is_empty() {
                models.extend(mask_model.iter().copied());
            }
            let estimate = ink.iter().map(|&m| pieces[m].0).reduce(|a, b| hull(&a, &b)).map(|hull_rect| {
                let mut mask = Mask::empty(hull_rect);
                for &m in &ink {
                    for &at in &island_pixels[m - components.len()] {
                        mask.set((at % width as usize) as i64, (at / width as usize) as i64, true);
                    }
                }
                mask
            });
            let split = (of > 1).then(|| SplitPart { index: k as u32, of, parent: id.clone() });
            groups.push(TextGroup {
                id: if of > 1 { format!("{id}-p{k}") } else { id.clone() },
                order: 0,
                origin: draft.origin,
                disposition: draft.disposition,
                bounds,
                anchor,
                bubble: draft.bubble.map(|w| wall_ids[w].clone()),
                bubble_rect: draft.bubble.map(|w| wall_rects[w]),
                component_ids: real.iter().map(|&m| component_id_of(inputs.pixel_model, m as u32 + 1)).collect(),
                box_ids: box_ids.clone(),
                models: models.into_iter().collect(),
                lettering_pixels: part.iter().map(|&m| pieces[m].1).sum(),
                score: draft.score,
                reasons: draft.reasons.iter().copied().collect(),
                split,
                estimated: estimate.is_some(),
                labels: real.iter().map(|&m| m as u32 + 1).collect(),
                estimate,
            });
        }
    }
    groups.sort_by(|a, b| {
        in_reading_order(&a.bounds, &b.bounds)
            .then(a.component_ids.cmp(&b.component_ids))
            .then(a.box_ids.cmp(&b.box_ids))
            .then(a.id.cmp(&b.id))
    });
    for (order, group) in groups.iter_mut().enumerate() {
        group.order = order as u32;
    }

    let mut out_components: Vec<PixelComponent> = components
        .iter()
        .enumerate()
        .map(|(index, (bounds, pixels))| PixelComponent {
            id: component_id_of(inputs.pixel_model, index as u32 + 1),
            bounds: *bounds,
            pixels: *pixels,
            group: None,
            via: owner[index].map(|(_, via)| via),
            reasons: component_reasons[index].iter().copied().collect(),
        })
        .collect();
    for group in &groups {
        for &label in &group.labels {
            out_components[label as usize - 1].group = Some(group.id.clone());
        }
    }
    Ok(Grouping {
        version: GROUPING_VERSION,
        width,
        height,
        origin: inputs.origin,
        pixel_model: inputs.pixel_model,
        models: {
            let mut models = inputs.models.clone();
            models.sort();
            models.dedup();
            models
        },
        components: out_components,
        boxes,
        groups,
        labels,
    })
}

/// Drafts in reading order of their lettering (their anchors, with none),
/// ties broken by their evidence, never by their index.
fn reading_sequence(drafts: &[Draft], components: &[(Rect, u32)]) -> Vec<usize> {
    let bounds: Vec<Rect> = drafts
        .iter()
        .map(|draft| {
            draft
                .lettering_bounds(components)
                .or_else(|| draft.anchors.iter().copied().reduce(|a, b| hull(&a, &b)))
                .unwrap_or(Rect::new(0, 0, 0, 0))
        })
        .collect();
    let mut sequence: Vec<usize> = (0..drafts.len()).collect();
    sequence.sort_by(|&a, &b| {
        in_reading_order(&bounds[a], &bounds[b])
            .then(drafts[a].members.cmp(&drafts[b].members))
            .then(drafts[a].box_indices.cmp(&drafts[b].box_indices))
    });
    sequence
}

/// Merge each draft into the one `into` names (following chains), and remap
/// `owner` to the surviving indices. The first lettered draft of a class
/// survives (else the lowest index does), and the others' lettering joins it.
/// Only [`within_reach`] names a lettered draft.
///
/// A class is confined to at most one balloon: a fold that would join drafts
/// confined to two (directly, or through a chain of unconfined boxes) is
/// refused, folds taken in draft order, and the survivor takes the class's
/// balloon.
fn fold(
    drafts: Vec<Draft>,
    into: &[Option<usize>],
    owner: &mut [Option<(usize, Assignment)>],
    boxes: &[DetectorBox],
) -> Vec<Draft> {
    if into.iter().all(Option::is_none) {
        return drafts;
    }
    let mut classes = Partition::new(drafts.len());
    let mut confined: Vec<Option<usize>> = drafts.iter().map(|draft| draft.bubble).collect();
    for (e, target) in into.iter().enumerate() {
        let Some(t) = *target else { continue };
        let (re, rt) = (classes.root(e), classes.root(t));
        if re == rt {
            continue;
        }
        if let (Some(a), Some(b)) = (confined[re], confined[rt]) {
            if a != b {
                continue;
            }
        }
        classes.union(e, t);
        confined[classes.root(e)] = confined[re].or(confined[rt]);
    }
    let mut remap = vec![0usize; drafts.len()];
    let mut slots: Vec<Option<Draft>> = drafts.into_iter().map(Some).collect();
    let mut merged: Vec<Draft> = Vec::new();
    for class in classes.classes() {
        let lettered = |d: &usize| slots[*d].as_ref().is_some_and(|draft| !draft.members.is_empty() || !draft.ink.is_empty());
        let keep = class.iter().copied().find(lettered).unwrap_or(class[0]);
        let mut survivor = slots[keep].take().expect("each draft is in one class");
        // The partition's root is its lowest index, the class's first member.
        survivor.bubble = confined[class[0]];
        for &d in &class {
            remap[d] = merged.len();
            if d == keep {
                continue;
            }
            let other = slots[d].take().expect("each draft is in one class");
            survivor.anchors.extend(other.anchors);
            survivor.box_indices.extend(other.box_indices);
            survivor.members.extend(other.members);
            survivor.ink.extend(other.ink);
            survivor.score = survivor.score.into_iter().chain(other.score).reduce(f32::max);
            survivor.reasons.extend(other.reasons);
        }
        survivor.box_indices.sort_unstable();
        survivor.box_indices.dedup();
        survivor.members.sort_unstable();
        if survivor.box_indices.iter().any(|&i| boxes[i].kind != BoxKind::CtdText) {
            survivor.origin = GroupOrigin::TextBox;
        }
        if !survivor.members.is_empty() {
            survivor.reasons.remove(&ReviewReason::MaskMissingUnderTextBox);
        }
        merged.push(survivor);
    }
    for slot in owner.iter_mut().flatten() {
        slot.0 = remap[slot.0];
    }
    merged
}

/// The reach two groups' lettering joins within: one larger glyph
/// ([`REACH_FRACTION`]), at most [`REACH_MAX`].
fn reach(glyph: i64) -> i64 {
    ((REACH_FRACTION * glyph as f64) as i64).clamp(2, REACH_MAX)
}

/// Which lettered cleaning drafts join which: two in the same balloon (or
/// both in none) whose lettering comes within [`reach`] of each other are
/// one. Returns the fold targets: each joined draft names its class's lowest
/// index.
///
/// Two limits keep a join from changing what the run does with the text:
///
/// - The detector's half of inside or outside is read at the region's centre
///   ([`crate::balloon::detected`], inside [`crate::balloon::in_bubble`]), so
///   a `text_bubble` box with no balloon box around it joins nothing: the
///   joined centre could leave its box and read as outside. Groups confined to
///   one balloon keep their centre in it.
/// - A joined group is at most [`crate::strip::DETECTION_OVERLAP`] on a side,
///   the tallest box a long strip's detection overlap holds whole, so one
///   segment still sees all of it.
#[allow(clippy::too_many_arguments)]
fn within_reach(
    drafts: &[Draft],
    owner: &[Option<(usize, Assignment)>],
    components: &[(Rect, u32)],
    labels: &[u32],
    boxes: &[DetectorBox],
    width: u32,
    height: u32,
) -> Vec<Option<usize>> {
    let bounds: Vec<Option<Rect>> = drafts
        .iter()
        .map(|d| d.lettering_bounds(components).filter(|_| d.disposition == Disposition::Clean))
        .collect();
    let glyphs: Vec<i64> = drafts.iter().map(|d| d.glyph(components)).collect();
    let loose_bubble_text =
        |d: &Draft| d.bubble.is_none() && d.box_indices.iter().any(|&i| boxes[i].kind == BoxKind::TextBubble);
    let limit = i64::from(crate::strip::DETECTION_OVERLAP);
    let mut classes = Partition::new(drafts.len());
    let mut hulls = bounds.clone();
    for a in 0..drafts.len() {
        let Some(ra) = bounds[a] else { continue };
        if loose_bubble_text(&drafts[a]) {
            continue;
        }
        for b in a + 1..drafts.len() {
            let Some(rb) = bounds[b] else { continue };
            if drafts[a].bubble != drafts[b].bubble || loose_bubble_text(&drafts[b]) {
                continue;
            }
            let (root_a, root_b) = (classes.root(a), classes.root(b));
            let (Some(ha), Some(hb)) = (hulls[root_a], hulls[root_b]) else { continue };
            let joined = hull(&ha, &hb);
            if root_a == root_b || i64::from(joined.w.max(joined.h)) > limit {
                continue;
            }
            let gap = reach(glyphs[a].max(glyphs[b]));
            let (gx, gy) = gaps(&ra, &rb);
            if gx.max(gy) <= gap && reaches((a, &ra), (b, &rb), gap, owner, labels, width, height) {
                classes.union(a, b);
                hulls[classes.root(a)] = Some(joined);
            }
        }
    }
    let mut into = vec![None; drafts.len()];
    for class in classes.classes() {
        for &d in &class[1..] {
            into[d] = Some(class[0]);
        }
    }
    into
}

/// Whether a lettering pixel of draft `a` lies within `gap` of one of draft
/// `b`'s, on both axes. Only the area within the gap of both groups' bounds
/// can hold such a pair, so `a`'s pixels there are grown by the gap (a
/// square, one axis at a time) and `b`'s are looked for in them.
fn reaches(
    (a, ra): (usize, &Rect),
    (b, rb): (usize, &Rect),
    gap: i64,
    owner: &[Option<(usize, Assignment)>],
    labels: &[u32],
    width: u32,
    height: u32,
) -> bool {
    let (ga, gb) = (grown_by(ra, gap, width, height), grown_by(rb, gap, width, height));
    let (x0, y0) = (ga.x.max(gb.x), ga.y.max(gb.y));
    let (x1, y1) = (ga.right().min(gb.right()), ga.bottom().min(gb.bottom()));
    if x1 <= x0 || y1 <= y0 {
        return false;
    }
    let (w, h, g) = ((x1 - x0) as usize, (y1 - y0) as usize, gap as usize);
    let draft_at = |x: usize, y: usize| {
        let label = labels[(y0 as usize + y) * width as usize + x0 as usize + x];
        label.checked_sub(1).and_then(|l| owner[l as usize]).map(|(d, _)| d)
    };
    // Along rows: whether an `a` pixel is within the gap.
    let mut near = vec![false; w * h];
    let mut sums = vec![0u32; w + 1];
    for y in 0..h {
        for x in 0..w {
            sums[x + 1] = sums[x] + u32::from(draft_at(x, y) == Some(a));
        }
        for x in 0..w {
            near[y * w + x] = sums[(x + g + 1).min(w)] > sums[x.saturating_sub(g)];
        }
    }
    // Along columns, looking for `b` as it goes.
    let mut sums = vec![0u32; h + 1];
    for x in 0..w {
        for y in 0..h {
            sums[y + 1] = sums[y] + u32::from(near[y * w + x]);
        }
        for y in 0..h {
            if sums[(y + g + 1).min(h)] > sums[y.saturating_sub(g)] && draft_at(x, y) == Some(b) {
                return true;
            }
        }
    }
    false
}

/// Attach loose components to a group beside them in the same balloon:
/// punctuation and strokes just outside an anchor, specks beside a layout
/// group. Each pass decides from the previous pass's state, so a pass's
/// result does not depend on the order components are visited in.
fn attach_by_proximity(
    drafts: &mut [Draft],
    owner: &mut [Option<(usize, Assignment)>],
    components: &[(Rect, u32)],
    here_of: &[Option<usize>],
) {
    for _ in 0..3 {
        let glyphs: Vec<i64> = drafts.iter().map(|d| d.glyph(components)).collect();
        let reach: Vec<Option<Rect>> = drafts
            .iter()
            .map(|d| {
                let lettering = d.lettering_bounds(components)?;
                Some(d.anchors.iter().fold(lettering, |acc, r| hull(&acc, r)))
            })
            .collect();
        let mut attached = Vec::new();
        for (index, (bounds, _)) in components.iter().enumerate() {
            if owner[index].is_some() {
                continue;
            }
            let best = (0..drafts.len())
                .filter(|&d| drafts[d].bubble == here_of[index] && !drafts[d].members.is_empty())
                .filter(|&d| {
                    let limit = glyphs[d] * 3 / 2;
                    (bounds.w as i64) <= limit.max(4) && (bounds.h as i64) <= limit.max(4)
                })
                .filter_map(|d| {
                    let (gx, gy) = gaps(bounds, reach[d].as_ref()?);
                    let gap = gx.max(gy);
                    let allowed = (glyphs[d] * 3 / 5).clamp(3, 32);
                    (gap <= allowed).then_some((gap, d))
                })
                .min();
            if let Some((_, d)) = best {
                attached.push((index, d));
            }
        }
        if attached.is_empty() {
            break;
        }
        for (index, d) in attached {
            owner[index] = Some((d, Assignment::Proximity));
            drafts[d].members.push(index);
        }
    }
    for draft in drafts.iter_mut() {
        draft.members.sort_unstable();
    }
}

/// The bounded ink estimate under text boxes the mask (almost) missed.
///
/// A cleaning group with text boxes whose mask lettering is under 1% of its
/// anchor (the old seeding rule; all of them, on a run with no pixel model)
/// takes, drafts in `sequence` order, the Otsu ink population of each box's
/// tight tier ([`crate::balloon::otsu_ink`], which refuses a box whose "ink"
/// would be most of it, or no population at all), less every mask pixel and
/// every pixel a group before it took. No pixel is claimed twice and none is
/// ever a whole box; the group keeps its mask lettering beside the estimate.
///
/// A box whose ink is mostly not free for its group (glyphs in another
/// group's mask, a duplicate box's estimate before it) claims nothing: what
/// would be left is halo and balloon outline, never a job of its own. A group
/// left with no lettering at all folds into the cleaning group of its own
/// balloon that holds most of that ink. Returns those fold targets; each
/// claimed pixel is on its draft's `ink`.
///
/// A group confined to a balloon neither claims nor counts ink outside its
/// wall (`walls`, each bubble box grown by the crossing margin): a text box
/// reaching over into the next balloon is never a job over that balloon's
/// lettering.
#[allow(clippy::too_many_arguments)]
fn estimate_ink(
    page: &crate::image::Raster,
    mask: Option<&[u8]>,
    labels: &[u32],
    owner: &[Option<(usize, Assignment)>],
    components: &[(Rect, u32)],
    boxes: &[DetectorBox],
    sequence: &[usize],
    walls: &[Rect],
    drafts: &mut [Draft],
) -> Vec<Option<usize>> {
    const UNOWNED: u32 = u32::MAX;
    let (width, height) = (page.width, page.height);
    // Who holds each pixel: 0 nobody, `d + 1` draft `d`, UNOWNED a mask pixel
    // no group holds.
    let mut held = vec![0u32; width as usize * height as usize];
    if let Some(mask) = mask {
        for (at, value) in mask.iter().enumerate() {
            if *value != 0 {
                held[at] = owner[labels[at] as usize - 1].map_or(UNOWNED, |(d, _)| d as u32 + 1);
            }
        }
    }
    let mut into = vec![None; drafts.len()];
    for &d in sequence {
        let draft = &drafts[d];
        if draft.disposition != Disposition::Clean || draft.box_indices.is_empty() {
            continue;
        }
        let lettered: u64 = draft.members.iter().map(|&m| u64::from(components[m].1)).sum();
        let anchor_area = draft.anchors.iter().copied().reduce(|a, b| hull(&a, &b)).map_or(0, |r| area(&r));
        if lettered as i64 >= (anchor_area / 100).max(4) {
            continue;
        }
        let me = d as u32 + 1;
        let wall = draft.bubble;
        let mut claimed: Vec<usize> = Vec::new();
        let mut votes: Vec<(usize, u64)> = Vec::new();
        for &i in &draft.box_indices {
            let tight = crate::detect::tight_rect(boxes[i].rect, width, height);
            let Some(ink) = crate::balloon::otsu_ink(page, tight) else { continue };
            let (mut free, mut own, mut total) = (Vec::new(), 0u64, 0u64);
            let mut others: Vec<(usize, u64)> = Vec::new();
            for y in tight.y..tight.bottom() {
                for x in tight.x..tight.right() {
                    if !ink.contains(x, y) || wall.is_some_and(|wall| !walls[wall].contains(x, y)) {
                        continue;
                    }
                    let at = y as usize * width as usize + x as usize;
                    total += 1;
                    match held[at] {
                        0 => free.push(at),
                        holder if holder == me => own += 1,
                        UNOWNED => {}
                        holder => match others.iter_mut().find(|(o, _)| *o == holder as usize - 1) {
                            Some(entry) => entry.1 += 1,
                            None => others.push((holder as usize - 1, 1)),
                        },
                    }
                }
            }
            if (free.len() as u64 + own) * 2 < total {
                for (other, count) in others {
                    match votes.iter_mut().find(|(o, _)| *o == other) {
                        Some(entry) => entry.1 += count,
                        None => votes.push((other, count)),
                    }
                }
                continue;
            }
            for at in free {
                held[at] = me;
                claimed.push(at);
            }
        }
        let bubble = draft.bubble;
        let pixel_less = draft.members.is_empty() && claimed.is_empty();
        claimed.sort_unstable();
        drafts[d].ink = claimed;
        if pixel_less {
            let top = votes.iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|&(t, _)| t);
            into[d] = top.filter(|&t| {
                drafts[t].disposition == Disposition::Clean && (bubble.is_none() || drafts[t].bubble == bubble)
            });
        }
    }
    into
}

/// The 8-connected islands of a set of raster pixels (row-major indices),
/// each in scan order.
fn ink_islands(pixels: &[usize], width: u32) -> Vec<Vec<usize>> {
    if pixels.is_empty() {
        return Vec::new();
    }
    let w = width as usize;
    let bounds = pixels_bounds(pixels, width);
    let (left, top, cw) = (bounds.x as usize, bounds.y as usize, bounds.w as usize);
    let mut local = vec![0u8; cw * bounds.h as usize];
    for &at in pixels {
        local[(at / w - top) * cw + at % w - left] = 1;
    }
    let labeled = label(bounds.w, bounds.h, &local);
    let mut islands = vec![Vec::new(); labeled.components.len()];
    for (i, &number) in labeled.labels.iter().enumerate() {
        if number != 0 {
            islands[number as usize - 1].push((i / cw + top) * w + i % cw + left);
        }
    }
    islands
}

/// The bounds of a nonempty set of raster pixels.
fn pixels_bounds(pixels: &[usize], width: u32) -> Rect {
    let w = width as usize;
    let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
    for &at in pixels {
        let (x, y) = (at % w, at / w);
        left = left.min(x);
        top = top.min(y);
        right = right.max(x + 1);
        bottom = bottom.max(y + 1);
    }
    Rect::new(left as i64, top as i64, (right - left) as u32, (bottom - top) as u32)
}

/// Text-layout adjacency between two glyph islands: same column, same row, or
/// punctuation beside a glyph, each bounded by glyph size and
/// [`LAYOUT_GAP_MAX`].
fn adjacent(a: &Rect, b: &Rect) -> bool {
    let (sa, sb) = (a.w.max(a.h) as i64, b.w.max(b.h) as i64);
    let (small, big) = (sa.min(sb), sa.max(sb));
    let limit = ((LAYOUT_GAP_FRACTION * big as f64) as i64).min(LAYOUT_GAP_MAX);
    let (gx, gy) = gaps(a, b);
    let comparable = big <= small * 4;
    let column = comparable && overlap_x(a, b) * 2 >= a.w.min(b.w) as i64 && gy <= limit;
    let row = comparable && overlap_y(a, b) * 2 >= a.h.min(b.h) as i64 && gx <= limit;
    let punctuation = small * 2 <= big && gx.max(gy) <= (big / 2).min(LAYOUT_GAP_MAX);
    column || row || punctuation
}

/// Split members so no part exceeds `max_side` on either axis. Each cut is
/// along the current long axis: the extent is cut into the fewest equal bands
/// that fit and each piece belongs to the band holding its centre; where a
/// band still overflows (a long piece), packing falls back to greedy. Each
/// part is split again until it fits, so a part is at most `max_side` square
/// unless it is a single piece larger than that.
fn split_members(members: &[usize], pieces: &[(Rect, u32)], max_side: u32) -> Vec<Vec<usize>> {
    let Some(bounds) = members.iter().map(|&m| pieces[m].0).reduce(|a, b| hull(&a, &b)) else {
        return vec![Vec::new()];
    };
    if (bounds.w <= max_side && bounds.h <= max_side) || members.len() == 1 {
        return vec![members.to_vec()];
    }
    split_along(members, pieces, max_side, bounds.h >= bounds.w)
        .into_iter()
        .flat_map(|part| {
            if part.len() < members.len() {
                split_members(&part, pieces, max_side)
            } else {
                vec![part]
            }
        })
        .collect()
}

/// One cut of [`split_members`], along the vertical axis or the horizontal.
fn split_along(members: &[usize], pieces: &[(Rect, u32)], max_side: u32, vertical: bool) -> Vec<Vec<usize>> {
    let start = |r: &Rect| if vertical { r.y } else { r.x };
    let end = |r: &Rect| if vertical { r.bottom() } else { r.right() };
    let low = members.iter().map(|&m| start(&pieces[m].0)).min().unwrap_or(0);
    let high = members.iter().map(|&m| end(&pieces[m].0)).max().unwrap_or(0);
    let extent = |part: &[usize]| {
        let s = part.iter().map(|&m| start(&pieces[m].0)).min().unwrap_or(0);
        let e = part.iter().map(|&m| end(&pieces[m].0)).max().unwrap_or(0);
        e - s
    };
    let bands = ((high - low) as u64).div_ceil(u64::from(max_side)).max(2) as i64;
    let band = (high - low + bands - 1) / bands;
    let mut parts: Vec<Vec<usize>> = vec![Vec::new(); bands as usize];
    for &m in members {
        let r = pieces[m].0;
        let centre = (start(&r) + end(&r)) / 2;
        parts[(((centre - low) / band.max(1)) as usize).min(bands as usize - 1)].push(m);
    }
    parts.retain(|part| !part.is_empty());
    if parts.len() > 1 && parts.iter().all(|part| extent(part) <= max_side as i64) {
        return parts;
    }
    let mut sorted = members.to_vec();
    sorted.sort_by_key(|&m| {
        let r = pieces[m].0;
        (start(&r) + end(&r), m)
    });
    let mut parts: Vec<Vec<usize>> = Vec::new();
    let mut span: Option<(i64, i64)> = None;
    for m in sorted {
        let r = pieces[m].0;
        match span.map(|(s, e)| (s.min(start(&r)), e.max(end(&r)))) {
            Some((s, e)) if e - s <= max_side as i64 => {
                parts.last_mut().expect("a span has a part").push(m);
                span = Some((s, e));
            }
            _ => {
                parts.push(vec![m]);
                span = Some((start(&r), end(&r)));
            }
        }
    }
    for part in &mut parts {
        part.sort_unstable();
    }
    parts
}

/* ------------------------------------------------------------------ */
/* Consumers                                                           */
/* ------------------------------------------------------------------ */

/// What a detection region carries about its group into the run loop.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionGroup {
    /// The exact glyph pixels, in the raster's coordinates.
    pub lettering: Mask,
    pub group: TextGroup,
}

impl Grouping {
    /// The group's exact lettering: the union of its components' pixels and
    /// its bounded ink estimate, if it has one.
    pub fn lettering(&self, group: &TextGroup) -> Mask {
        let pixels = lettering_of(&self.labels, self.width, &self.components_bounds(), &group.labels, group.bounds);
        with_estimate(pixels, group.estimate.as_ref(), self.width, self.height)
    }

    /// The raster's component labels (0 background, else 1-based), as
    /// [`label`] gave them; empty when no mask was grouped.
    pub(crate) fn labels(&self) -> &[u32] {
        &self.labels
    }

    fn components_bounds(&self) -> Vec<Rect> {
        self.components.iter().map(|c| c.bounds).collect()
    }

    /// Groups that become cleaning jobs with a mask's lettering.
    pub fn cleaning(&self) -> impl Iterator<Item = &TextGroup> {
        self.groups.iter().filter(|g| g.disposition == Disposition::Clean && !g.component_ids.is_empty())
    }

    /// Anchored groups with no mask pixels under them. Their lettering is the
    /// bounded ink estimate, when there is one.
    pub fn detector_only(&self) -> impl Iterator<Item = &TextGroup> {
        self.groups.iter().filter(|g| g.disposition == Disposition::Clean && g.component_ids.is_empty())
    }

    /// Paint each cleaning group's ink estimate into `segmentation`, the
    /// raster's own: the gate reads lines off the segmentation, and an
    /// estimate is its group's lettering there as a mask's pixels are.
    pub fn paint_estimates(&self, segmentation: &mut crate::detect::Segmentation) {
        let width = segmentation.width as usize;
        let cleaned = self.groups.iter().filter(|group| group.disposition == Disposition::Clean);
        for estimate in cleaned.filter_map(|group| group.estimate.as_ref()) {
            for y in estimate.bounds.y..estimate.bounds.bottom() {
                for x in estimate.bounds.x..estimate.bounds.right() {
                    if estimate.contains(x, y) {
                        segmentation.levels[y as usize * width + x as usize] = 255;
                    }
                }
            }
        }
    }

    /// Groups listed and left untouched.
    pub fn candidates(&self) -> impl Iterator<Item = &TextGroup> {
        self.groups.iter().filter(|g| g.disposition == Disposition::Candidate)
    }

    /// The block-size reference for the unusually-large flag: the median
    /// lettering area of anchored cleaning groups, never of per-glyph boxes.
    pub fn block_median_area(&self) -> i64 {
        let rects: Vec<Rect> = self
            .cleaning()
            .filter(|g| g.origin != GroupOrigin::MaskLayout)
            .map(|g| g.bounds)
            .collect();
        let rects = if rects.is_empty() { self.cleaning().map(|g| g.bounds).collect() } else { rects };
        crate::detect::median_rect_area(&rects)
    }

    /// One detection region for a cleaning group, grown through the same tiers
    /// as a detector box, carrying the exact lettering as its seed.
    pub fn region(&self, group: &TextGroup, median_area: i64) -> Region {
        let mut region = Region::from_box(
            group.bounds,
            group.score.unwrap_or(1.0),
            DetectedLanguage::Japanese,
            self.width,
            self.height,
            0,
        );
        region.flagged_large = crate::detect::block_size_verdict(
            &group.bounds,
            median_area,
            self.width,
            self.height,
        ) == SizeVerdict::FlagLarge;
        region.group = Some(Arc::new(RegionGroup { lettering: self.lettering(group), group: group.clone() }));
        region
    }

    /// Anchor regions for groups with no mask lettering: the box itself (a
    /// split part's own lettering bounds), seeded by the group's ink estimate
    /// (empty when there is none).
    pub fn detector_only_region(&self, group: &TextGroup, median_area: i64) -> Region {
        let rect = if group.split.is_some() { group.bounds } else { group.anchor.unwrap_or(group.bounds) };
        let mut region = Region::from_box(
            rect,
            group.score.unwrap_or(1.0),
            DetectedLanguage::Japanese,
            self.width,
            self.height,
            0,
        );
        region.flagged_large =
            crate::detect::block_size_verdict(&rect, median_area, self.width, self.height) == SizeVerdict::FlagLarge;
        let lettering = match &group.estimate {
            Some(estimate) => estimate.clone(),
            None => Mask::empty(Rect::new(rect.x, rect.y, 0, 0)),
        };
        region.group = Some(Arc::new(RegionGroup { lettering, group: group.clone() }));
        region
    }

    /// The persisted record of `group`, in page coordinates ([`Inputs::origin`]
    /// is the raster's place on the page).
    pub fn record(&self, group: &TextGroup, evidence: Option<&EvidenceBlob>) -> GroupRecord {
        let (dx, dy) = self.origin;
        let shift = |r: Rect| Rect::new(r.x + dx, r.y + dy, r.w, r.h);
        let lettering = self.lettering(group);
        let lettering_sha256 = format!("{:x}", Sha256::digest(crate::project::buffers::encode_mask(&Mask {
            bounds: shift(lettering.bounds),
            bits: lettering.bits,
        })));
        let boxes = group
            .box_ids
            .iter()
            .filter_map(|id| self.boxes.iter().find(|b| &b.id == id))
            .map(|b| DetectorBox { rect: shift(b.rect), ..b.clone() })
            .collect();
        GroupRecord {
            version: GROUPING_VERSION,
            id: group.id.clone(),
            origin: group.origin,
            bounds: shift(group.bounds),
            lettering_pixels: group.lettering_pixels,
            lettering_sha256,
            components: group.component_ids.clone(),
            boxes,
            bubble: group.bubble_rect.map(shift),
            models: self.models.iter().copied().filter(|m| group.models.contains(&m.model)).collect(),
            reasons: group.reasons.clone(),
            split: group.split.clone(),
            estimated: group.estimated,
            evidence_ref: evidence.map(|e| e.reference()),
            evidence_sha256: evidence.map(|e| e.sha256.clone()),
        }
    }

    /// The evidence file for this grouping and the pixel mask it was made
    /// from (`None` when no model gave pixels).
    pub fn evidence(&self, mask: Option<&[u8]>) -> Result<EvidenceBlob, String> {
        let blank;
        let mask = match mask {
            Some(mask) => mask,
            None => {
                blank = vec![0u8; self.width as usize * self.height as usize];
                &blank
            }
        };
        EvidenceBlob::encode(self, mask)
    }

    /// Which group's estimate owns each pixel: its index plus one, 0 for none.
    fn estimate_owners(&self) -> Result<Option<Vec<u16>>, String> {
        if !self.groups.iter().any(|g| g.estimate.is_some()) {
            return Ok(None);
        }
        let mut owners = vec![0u16; self.width as usize * self.height as usize];
        for (index, group) in self.groups.iter().enumerate() {
            let Some(estimate) = &group.estimate else { continue };
            let owner = u16::try_from(index + 1).map_err(|_| "too many groups for estimate evidence")?;
            for y in estimate.bounds.y..estimate.bounds.bottom() {
                for x in estimate.bounds.x..estimate.bounds.right() {
                    if estimate.contains(x, y) {
                        let at = y as usize * self.width as usize + x as usize;
                        if owners[at] != 0 {
                            return Err("two estimates claim one pixel".into());
                        }
                        owners[at] = owner;
                    }
                }
            }
        }
        Ok(Some(owners))
    }

    /// Rebuilds each estimated group's lettering from an owner plane.
    fn restore_estimates(&mut self, owners: &[u16]) -> Result<(), String> {
        let width = self.width as usize;
        let mut pixels: Vec<Vec<usize>> = vec![Vec::new(); self.groups.len()];
        for (at, &owner) in owners.iter().enumerate() {
            if owner == 0 {
                continue;
            }
            pixels.get_mut(owner as usize - 1).ok_or("estimate owner outside the groups")?.push(at);
        }
        let component_pixels: Vec<u32> = self.components.iter().map(|c| c.pixels).collect();
        for (group, claimed) in self.groups.iter_mut().zip(pixels) {
            let from_mask: u64 = group
                .labels
                .iter()
                .map(|&l| component_pixels.get(l as usize - 1).copied().map(u64::from))
                .sum::<Option<u64>>()
                .ok_or("estimate evidence names a component outside it")?;
            if claimed.is_empty() == group.estimated
                || (group.estimated && claimed.len() as u64 + from_mask != u64::from(group.lettering_pixels))
            {
                return Err("estimate evidence does not match its groups".into());
            }
            if claimed.is_empty() {
                continue;
            }
            let (xs, ys) = (claimed.iter().map(|&at| (at % width) as i64), claimed.iter().map(|&at| (at / width) as i64));
            let (left, right) = (xs.clone().min().unwrap_or(0), xs.max().unwrap_or(0) + 1);
            let (top, bottom) = (ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0) + 1);
            let mut estimate = Mask::empty(Rect::new(left, top, (right - left) as u32, (bottom - top) as u32));
            for &at in &claimed {
                estimate.set((at % width) as i64, (at / width) as i64, true);
            }
            group.estimate = Some(estimate);
        }
        Ok(())
    }
}

/// `pixels` with `estimate` added.
fn with_estimate(pixels: Mask, estimate: Option<&Mask>, width: u32, height: u32) -> Mask {
    match estimate {
        None => pixels,
        Some(estimate) if pixels.is_empty() => estimate.clone(),
        Some(estimate) => Mask::union(&[&pixels, estimate], width, height),
    }
}

fn lettering_of(labels: &[u32], width: u32, component_bounds: &[Rect], members: &[u32], fallback: Rect) -> Mask {
    let Some(bounds) = members
        .iter()
        .map(|&l| component_bounds[l as usize - 1])
        .reduce(|a, b| hull(&a, &b))
    else {
        return Mask::empty(Rect::new(fallback.x, fallback.y, 0, 0));
    };
    let wanted: BTreeSet<u32> = members.iter().copied().collect();
    let mut mask = Mask::empty(bounds);
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if wanted.contains(&labels[y as usize * width as usize + x as usize]) {
                mask.set(x, y, true);
            }
        }
    }
    mask
}

/// A group as the manifest keeps it on its detection row. Page coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupRecord {
    pub version: u32,
    /// Exact-evidence group identity, `tg-...` (see the module notes): unique
    /// on its page, stable only for identical evidence. Not the region id.
    pub id: String,
    pub origin: GroupOrigin,
    /// Lettering bounds.
    pub bounds: Rect,
    pub lettering_pixels: u32,
    /// SHA-256 of the exact lettering mask in the sidecar mask encoding
    /// (`project::buffers::encode_mask`), page coordinates.
    pub lettering_sha256: String,
    /// Component ids in the evidence file.
    pub components: Vec<String>,
    /// Contributing detector boxes, raw, page coordinates.
    pub boxes: Vec<DetectorBox>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bubble: Option<Rect>,
    pub models: Vec<ModelUse>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<ReviewReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<SplitPart>,
    /// The lettering is an ink estimate under the text boxes, not model pixels.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
    /// `evidence/<sha256>.mtev` within the job sidecar directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_sha256: Option<String>,
}

impl GroupRecord {
    /// The `review.reason.*` key a layer is flagged under, if the evidence
    /// gives one. The first reason in declaration order wins. That no box
    /// claimed the lettering is where it came from, not a flag: the mask
    /// decides what is lettering, so the record keeps it and review does not.
    pub fn review_key(&self) -> Option<&'static str> {
        self.reasons
            .iter()
            .filter(|reason| !matches!(reason, ReviewReason::UnassignedComponent | ReviewReason::IsolatedComponent))
            .min()
            .map(|reason| reason.key())
    }

    /// The compact provenance object patch snapshots carry. `modelsDescribed`
    /// names each model's checkpoint, revision, execution and spatial input
    /// ([`ModelUse::identity`]) beside the bare `models` tags.
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "version": self.version,
            "origin": self.origin,
            "components": self.components.len(),
            "boxes": self.boxes.iter().map(|b| b.id.clone()).collect::<Vec<_>>(),
            "models": self.models,
            "modelsDescribed": self.models.iter().map(ModelUse::identity).collect::<Vec<_>>(),
            "reasons": self.reasons,
            "estimated": self.estimated,
            "letteringPixels": self.lettering_pixels,
            "letteringSha256": self.lettering_sha256,
            "evidence": self.evidence_ref,
        })
    }
}

/* ------------------------------------------------------------------ */
/* Evidence file                                                       */
/* ------------------------------------------------------------------ */

const EVIDENCE_MAGIC: &[u8; 5] = b"MTEV2";
/// Grouping version 1 files: no estimates, the PNG runs to the end.
const EVIDENCE_MAGIC_V1: &[u8; 5] = b"MTEV1";

/// One grouping's evidence, encoded: `MTEV2`, a little-endian `u32` JSON
/// length, the JSON ([`Grouping`] without labels), a little-endian `u32` PNG
/// length, the pixel mask as an 8-bit grayscale PNG of the raster, then, when
/// any group's lettering is an ink estimate, a 16-bit grayscale PNG owner
/// plane (group index plus one, 0 for none). Content-addressed by `sha256`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceBlob {
    pub sha256: String,
    pub bytes: Arc<Vec<u8>>,
}

fn encode_png(width: u32, height: u32, depth: png::BitDepth, data: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(depth);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(data).map_err(|e| e.to_string())?;
    }
    Ok(bytes)
}

fn decode_png(bytes: &[u8], width: u32, height: u32, sample_bytes: usize) -> Result<Vec<u8>, String> {
    let size = width as usize * height as usize * sample_bytes;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits { bytes: size.max(1) });
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut data = vec![0u8; size];
    let frame = reader.next_frame(&mut data).map_err(|e| e.to_string())?;
    if frame.width != width || frame.height != height || frame.buffer_size() != size {
        return Err("evidence mask geometry mismatch".into());
    }
    Ok(data)
}

impl EvidenceBlob {
    fn encode(grouping: &Grouping, mask: &[u8]) -> Result<EvidenceBlob, String> {
        let (width, height) = (grouping.width, grouping.height);
        if mask.len() != width as usize * height as usize {
            return Err("evidence mask dimensions differ from the grouping".into());
        }
        let json = serde_json::to_vec(grouping).map_err(|e| e.to_string())?;
        let binary: Vec<u8> = mask.iter().map(|&v| if v != 0 { 255 } else { 0 }).collect();
        let png_bytes = encode_png(width, height, png::BitDepth::Eight, &binary)?;
        let owners = match grouping.estimate_owners()? {
            Some(owners) => {
                let big_endian: Vec<u8> = owners.iter().flat_map(|o| o.to_be_bytes()).collect();
                encode_png(width, height, png::BitDepth::Sixteen, &big_endian)?
            }
            None => Vec::new(),
        };
        let mut bytes = Vec::with_capacity(13 + json.len() + png_bytes.len() + owners.len());
        bytes.extend_from_slice(EVIDENCE_MAGIC);
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&json);
        bytes.extend_from_slice(&(png_bytes.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&png_bytes);
        bytes.extend_from_slice(&owners);
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        Ok(EvidenceBlob { sha256, bytes: Arc::new(bytes) })
    }

    /// Where the file lives relative to the job sidecar directory.
    pub fn reference(&self) -> String {
        format!("evidence/{}.mtev", self.sha256)
    }
}

/// A decoded evidence file.
pub struct EvidenceFile {
    pub grouping: Grouping,
    pub mask: Vec<u8>,
}

impl EvidenceFile {
    pub fn decode(bytes: &[u8]) -> Result<EvidenceFile, String> {
        let version_two = bytes.get(..5) == Some(EVIDENCE_MAGIC.as_slice());
        if bytes.len() < 9 || !(version_two || &bytes[..5] == EVIDENCE_MAGIC_V1) {
            return Err("not a text-group evidence file".into());
        }
        let length = u32::from_le_bytes(bytes[5..9].try_into().unwrap()) as usize;
        let json = bytes.get(9..9 + length).ok_or("truncated evidence file")?;
        let mut grouping: Grouping = serde_json::from_slice(json).map_err(|e| e.to_string())?;
        let rest = &bytes[9 + length..];
        let (png_bytes, owner_bytes) = if version_two {
            let size = rest.get(..4).ok_or("truncated evidence file")?;
            let size = u32::from_le_bytes(size.try_into().unwrap()) as usize;
            let png_bytes = rest.get(4..4 + size).ok_or("truncated evidence file")?;
            (png_bytes, &rest[4 + size..])
        } else {
            (rest, &rest[rest.len()..])
        };
        let (width, height) = (grouping.width, grouping.height);
        let mask = decode_png(png_bytes, width, height, 1)?;
        let labeled = label(width, height, &mask);
        if labeled.components.len() != grouping.components.len() {
            return Err("evidence components do not match its mask".into());
        }
        grouping.labels = labeled.labels;
        for group in &mut grouping.groups {
            group.labels = group.component_ids.iter().filter_map(|id| component_label(id)).collect();
        }
        if owner_bytes.is_empty() {
            if grouping.groups.iter().any(|g| g.estimated) {
                return Err("estimate evidence is missing".into());
            }
        } else {
            let plane = decode_png(owner_bytes, width, height, 2)?;
            let owners: Vec<u16> = plane.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect();
            grouping.restore_estimates(&owners)?;
        }
        Ok(EvidenceFile { grouping, mask })
    }

    /// A recorded group's exact lettering, in page coordinates.
    pub fn lettering(&self, record: &GroupRecord) -> Result<Mask, String> {
        let g = &self.grouping;
        let members: Vec<u32> = record
            .components
            .iter()
            .map(|id| component_label(id).ok_or("bad component id"))
            .collect::<Result<_, _>>()?;
        if members.iter().any(|&l| l == 0 || l as usize > g.components.len()) {
            return Err("component id outside the evidence".into());
        }
        let pixels = lettering_of(&g.labels, g.width, &g.components_bounds(), &members, record.bounds);
        let local = if record.estimated {
            let estimate = g
                .groups
                .iter()
                .find(|group| group.id == record.id)
                .and_then(|group| group.estimate.as_ref())
                .ok_or("estimated group missing from the evidence")?;
            with_estimate(pixels, Some(estimate), g.width, g.height)
        } else {
            pixels
        };
        let (dx, dy) = g.origin;
        Ok(Mask { bounds: Rect::new(local.bounds.x + dx, local.bounds.y + dy, local.bounds.w, local.bounds.h), bits: local.bits })
    }
}

#[cfg(test)]
mod measure;
#[cfg(test)]
mod tests;
