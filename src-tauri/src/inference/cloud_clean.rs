//! Cloud clean: stored detections rendered by FLUX.2 Klein on the user's own
//! Modal or Beam GPU, as one plan a person consented to
//! (`docs/detect-clean.md` §3).
//!
//! ## Three calls, and nothing sent before the third
//!
//! 1. [`prepare`] describes the work and does none of it. It collects the
//!    detections in scope (a page, the chapter, or listed ids) in page and
//!    reading order, leaves out every region an earlier cloud request of which
//!    is still unresolved and every region whose crop the render service
//!    would refuse even with no page around it, and states the plan: the
//!    exact ordered region ids,
//!    the chunking rule ([`CHUNK_REGIONS`]), each job's crop and the estimate
//!    built from those crops, and where the regions are cleaned
//!    ([`CleanExecution`]). It reads the gateway's model identity and GPU
//!    price, sends no page data, and writes nothing.
//! 2. [`confirm`] takes the two consent answers and the plan digest the
//!    consent dialog showed ([`compute_clean_plan_digest`]: the chapter, the
//!    ordered ids and each region's revision hash (source, record and masks),
//!    the chunking rule, the estimate, the execution, the provider, profile,
//!    endpoint and recipe). Only when that digest is the proposal's own does
//!    it mint a **clean grant**: single use, valid for [`CLEAN_GRANT_TTL`],
//!    and holding that plan.
//! 3. `start_cloud_clean` spends it ([`consume_grant`]) and runs the plan in
//!    the ordinary run slot ([`crate::run::start_in_slot`]): the same events,
//!    the same `cancel_run`, one run at a time.
//!
//! ## The cloud, unless mixed was chosen
//!
//! Clean on Cloud GPU means the cloud, whatever each region's pick: a `lama`
//! pick is rendered there like any other, because the pick names a local
//! engine and the user asked for the cloud. No region is cleaned by LaMa here.
//!
//! [`CleanExecution::Cloud`] is the default and what the interface asks for:
//! every region in the plan is rendered on the cloud GPU. [`CleanExecution::Mixed`]
//! is the explicit alternative: after the run starts, just before each chunk
//! goes to the cloud, the chunk's `fill` and `solid` picks are tried on this
//! computer first, a page at a time ([`crate::run::clean_fill_rung_first`]),
//! and only what that leaves goes on. The consent and its cost range still
//! cover every such region as sent, so mixed only ever sends less.
//!
//! ## One consent, bounded chunks
//!
//! The run cuts the plan into consecutive chunks of at most [`CHUNK_REGIONS`]
//! ([`chunk_range`]), never more than the [`MAX_CLEAN_REGIONS`] one chunk may
//! carry. Before each chunk it checks its authority again (cloud engines on,
//! the profile still selected, its epoch unchanged, the deployment's recipe
//! and GPU the ones the estimate priced) and stops the plan if any has moved.
//! It then mints the chunk's own grant from the plan's list
//! ([`issue_chunk_grant`]), leaving out any region whose earlier cloud request
//! has become unresolved since, and proves it a subset of the plan
//! ([`verify_chunk_grant`]) before one region of it starts. Cancelling stops
//! the chunk in flight and every chunk after it.
//!
//! ## Each region is its own render
//!
//! The run does not send anything the grant only named. Every region is read
//! again from disk ([`read_render_input`]) and its revision hash compared with
//! the one consent bound: a region edited or deleted since is skipped with a
//! notice, never rendered from what it used to be. A region that still matches
//! mints and spends its own [`GrantService`] grant against the epoch captured at
//! consent and goes through the one render path every cloud render uses
//! ([`InferenceService::execute_cloud_render`]): the journal, recovery, the
//! `cloud://attempt` phases and the commit through
//! [`cleaner_core::project::Job::complete_detection`] are that path's, not
//! this module's. The commit keeps the detection's text group on its patch.
//!
//! A region that fails stays detected, with a notice. It is not retried and
//! not cleaned locally instead. A failure that withdraws the run's authority
//! ([`stops_run`], a changed profile epoch, cloud switched off, another profile
//! selected) stops the run, and the renders in flight are cancelled.
//!
//! ## Three in flight, never two that read each other
//!
//! Up to [`MAX_IN_FLIGHT`] renders run at once, so the GPU is not left waiting
//! on the network between jobs. Two regions of one page are not always
//! independent, though: a render's input is the page composited with every
//! patch below it within [`crate::underlay::READ_MARGIN`], and the attach
//! refuses a result whose underlay changed while it rendered. A region
//! therefore waits while a region below it on the same page and within reach
//! is still to be committed ([`next_ready`]). In practice regions of one page
//! go one after another and the three slots are spent on different pages.
//! A long strip is one page for this purpose: a render there reads across a
//! join ([`crate::underlay::prepare_cloud_seed`]), so the regions are placed
//! in strip pixels and a region waits for one below it on either side of a
//! join. A chunk ends before the next begins, so a region never waits on a
//! later one.
//!
//! After the last chunk, or a stop over a changed deployment or a refused
//! chunk, the run asks for an idle-only release of the render GPU it woke, so
//! the idle window is not billed; any failure is ignored and the GPU scales
//! down on its own timer. Only a run that no longer speaks for the endpoint
//! ([`withdraws_authority`]) asks it nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use cleaner_core::engines::render::{
    CloudProvider, ExecutionTarget, Preprocessing, RenderRecipe, CONTEXT_WIDE, LATENT_STRIDE,
};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::mask::Rect;
use cleaner_core::project::{Job, StripMode};
use serde::Serialize;

use crate::events::{self, Event};
use crate::inference::analysis;
use crate::inference::commands::{app_inference_service, build_client_for_profile, BuildClientError};
use crate::inference::consent::{
    chunk_count, chunk_range, compute_clean_plan_digest, compute_operation_digest, issue_chunk_grant,
    load_cloud_region, verify_chunk_grant, ChunkGrant, CleanPlanTerms, CloudRegionKind, ConsentError,
    OperationIntent, PlanEstimateTerms, PlanRegionTerms,
};
use crate::inference::gpu::{self, GpuRole, GpuStatusWire};
use crate::inference::http::CloudHttpClient;
use crate::inference::policy::{GrantError, GrantService};
use crate::inference::run_analysis::stops_run;
use crate::inference::service::{
    attempt_id_for_nonce, read_render_input, request_render_cancel, InferenceService, InferenceServiceError,
    PollOptions, RenderInput,
};
use crate::inference::InferenceConfig;
use crate::library::{self, ApiRegion, Library};
use crate::run::{self, QueuedPage, RunHandle, Summary};

/// How long a confirmed consent may wait for its run. It bounds the start
/// only: once spent, the run holds the plan for as long as it takes.
pub const CLEAN_GRANT_TTL: Duration = Duration::from_secs(120);
/// The most regions one chunk grant may carry: the transport bound on a
/// single consented batch, unchanged. A plan is cut into chunks under it.
pub const MAX_CLEAN_REGIONS: usize = 1024;
/// The chunking rule: consecutive runs of at most this many regions, in plan
/// order. Small enough that authority is checked again every few minutes of
/// GPU time; bound into the plan digest.
pub const CHUNK_REGIONS: usize = 256;
const _: () = assert!(CHUNK_REGIONS > 0 && CHUNK_REGIONS <= MAX_CLEAN_REGIONS);
/// The most regions one plan covers, checked before a region is read.
pub const MAX_PLAN_REGIONS: usize = 16 * MAX_CLEAN_REGIONS;
/// Renders in flight at once.
pub const MAX_IN_FLIGHT: usize = 3;
/// Each region's own grant lives only from its mint to its dispatch.
const REGION_GRANT_TTL: Duration = Duration::from_secs(60);
const MAX_OPEN_PROPOSALS: usize = 16;
const MAX_OPEN_GRANTS: usize = 16;
/// How often the run looks at its cancel flag while renders are out.
const TICK: Duration = Duration::from_millis(50);
/// How far a committed patch reaches into another region's render input: the
/// underlay read margin, plus the widest context a crop adds, since a rendered
/// mask may grow to its crop.
const DEPENDENCY_REACH: i64 = crate::underlay::READ_MARGIN + CONTEXT_WIDE as i64;

/* Cost. Each job is priced from its own crop: the crop the render cuts around
 * the region's stored lettering (`CloudRegion::estimated_crop`), at the size the worker edits it
 * ([`working_size`]). A job costs a fixed overhead plus its working pixels at
 * a per-megapixel rate; the low end is the sum over the plan, and the high end
 * adds a cold start and the idle window a GPU bills before it scales down.
 * Priced at the deployment's GPU list price plus the worker's CPU and memory,
 * at Modal's published rates. Both per-job rates are **not measured** (see
 * docs/findings.md), so the range is a guide, not a quote. */

/// The long side the FLUX.2 Klein worker edits a smaller crop at
/// (`WORK_LONG_SIDE`, deploy/cloud/common/manifest.py). Larger crops are
/// edited at their own size.
pub const WORK_LONG_SIDE: u32 = 768;
/// GPU seconds a job costs before its pixels: decode, encode, transfer. Unmeasured.
pub const JOB_OVERHEAD_SECONDS: f64 = 2.0;
/// GPU seconds per working megapixel at the pinned four steps. Unmeasured:
/// chosen so a 768 by 768 job costs the 8 s the old per-region figure did.
pub const SECONDS_PER_WORK_MEGAPIXEL: f64 = 10.0;
/// A cold container: image, snapshot restore and weights.
pub const COLD_START_SECONDS: f64 = 55.0;
/// The Qwen-Image-Edit model family (deploy/cloud/common/qwen.py).
const QWEN_MODEL_PREFIX: &str = "Disty0/Qwen-Image-Edit";
/// The reference area the Qwen pipeline edits every crop at, on a 32 px grid.
const QWEN_WORK_AREA: f64 = 1024.0 * 1024.0;
const QWEN_GRID: f64 = 32.0;

/// What one deployment's worker costs to run, from the model its recipe names.
/// Measured for Qwen on an L40S (docs/research/qwen-image-edit-2511-cloud-plan.md):
/// 7.4 s per one-megapixel crop warm, 33 s to load warm weights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RenderRate {
    qwen: bool,
    seconds_per_work_megapixel: f64,
    cold_start_seconds: f64,
    /// The render container's host memory on Modal and on Beam
    /// (`render_worker_memory_gib`, deploy/cloud/common/capacity.py).
    memory_gib: (f64, f64),
}

impl RenderRate {
    /// FLUX.2 Klein 4B fits two pipeline copies on every allowed GPU
    /// (`render_copies`), so its container holds two: Modal loads the second in
    /// the same process (12 GiB plus a measured 7 GiB), Beam runs it as a
    /// second 12 GiB worker. The 9B and Qwen hold two only on an L40S, which is
    /// the GPU Modal pins them to (24 GiB plus 24, 32 plus 24); on Beam's RTX5090
    /// they hold one.
    ///
    /// The seconds are unchanged by the second copy. They are GPU seconds
    /// summed over the plan, and two renders at once share one GPU's compute;
    /// how much the overlap saves is not measured.
    pub(crate) fn of(recipe: &RenderRecipe) -> Self {
        if recipe.model_id.starts_with(QWEN_MODEL_PREFIX) {
            // Eight steps: a linear estimate from the measured four-step rate.
            return Self { qwen: true, seconds_per_work_megapixel: 14.0, cold_start_seconds: 90.0, memory_gib: (56.0, 32.0) };
        }
        let memory_gib = if recipe.model_id.contains("9B") { (48.0, 24.0) } else { (19.0, 24.0) };
        Self { qwen: false, seconds_per_work_megapixel: SECONDS_PER_WORK_MEGAPIXEL,
            cold_start_seconds: COLD_START_SECONDS, memory_gib }
    }

    /// The render container's host memory on `provider`.
    pub(crate) fn memory_gib(&self, provider: CloudProvider) -> f64 {
        match provider {
            CloudProvider::Modal => self.memory_gib.0,
            CloudProvider::Beam => self.memory_gib.1,
        }
    }

    /// The size the worker edits a `w` by `h` crop at.
    pub(crate) fn working_size(&self, w: u32, h: u32) -> (u32, u32) {
        if !self.qwen { return working_size(w, h); }
        // calculate_dimensions in diffusers' Qwen edit pipeline.
        let ratio = f64::from(w.max(1)) / f64::from(h.max(1));
        let width = (QWEN_WORK_AREA * ratio).sqrt();
        let snap = |extent: f64| ((extent / QWEN_GRID).round() * QWEN_GRID) as u32;
        (snap(width), snap(width / ratio))
    }
}
/// The deployment permits up to 600 idle seconds. The gateway does not report
/// the configured idle setting when no container is running, so high uses the
/// maximum rather than a default that can understate the idle tail.
pub const IDLE_WINDOW_SECONDS: f64 = 600.0;
/// The render worker's CPU (`cpu=2.0`).
const CPU_CORES: f64 = 2.0;
const CPU_USD_PER_CORE_SECOND: f64 = 0.000_013_1;
const MEMORY_USD_PER_GIB_SECOND: f64 = 0.000_002_22;

/* ------------------------------------------------------------------ */
/* Where Clean runs                                                    */
/* ------------------------------------------------------------------ */

/// The `cleanTarget` setting: whether the interface's Clean goes to this
/// computer's ladder or to a consented cloud batch. The backend acts on no
/// setting here (a cloud clean starts only from its own three calls); it only
/// keeps the value well formed, since it decides where pages are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CleanTarget {
    #[default]
    Local,
    Cloud,
}

pub const CLEAN_TARGET_KEY: &str = "cleanTarget";

impl CleanTarget {
    /// Strict: `"local"` or `"cloud"`. Absent or null is local. Anything else
    /// is refused, because a typo must not decide where a region is sent.
    pub fn from_value(value: Option<&serde_json::Value>) -> Result<Self, String> {
        match value.filter(|value| !value.is_null()).map(|value| value.as_str()) {
            None | Some(Some("local")) => Ok(Self::Local),
            Some(Some("cloud")) => Ok(Self::Cloud),
            Some(_) => Err("cleanTarget must be \"local\" or \"cloud\"".into()),
        }
    }
}

/// Where a consented plan's regions are cleaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CleanExecution {
    /// Every region on the cloud GPU; nothing on this computer.
    #[default]
    Cloud,
    /// Chosen explicitly (`localFirst`): after the run starts, `fill` and
    /// `solid` picks are tried on this computer first, the rest on the cloud.
    Mixed,
}

impl CleanExecution {
    fn key(self) -> &'static str {
        match self {
            Self::Cloud => "cloud",
            Self::Mixed => "mixed",
        }
    }
}

/* ------------------------------------------------------------------ */
/* Proposal and grant                                                  */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CostRange {
    pub low: f64,
    pub high: f64,
}

/// What a cloud clean would send. `proposalId` is `None`, and `regions` 0,
/// when nothing in scope can go: there was nothing, or every region in scope
/// has an unresolved earlier request (`unresolvedIds`) or is too large to
/// send (`tooLargeIds`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCleanProposal {
    pub proposal_id: Option<String>,
    pub chapter_id: String,
    /// Every region the consent covers, in the order the run takes them.
    pub region_ids: Vec<String>,
    pub regions: u32,
    /// How many pages `pageIndices` names.
    pub pages: u32,
    pub page_indices: Vec<u32>,
    /// Each job's crop as the deployment's recipe's preprocessing sizes it
    /// from the stored region, the crop its render sends.
    pub total_crop_pixels: u64,
    /// The same crops at the size the worker edits them ([`working_size`]).
    pub total_work_pixels: u64,
    /// Always 0: preparing cleans nothing. Kept for callers that read it.
    pub local_cleaned: u32,
    pub execution: CleanExecution,
    /// Mixed only: the `fill` and `solid` picks the run tries here first.
    pub local_candidates: u32,
    /// The chunking rule, and how many chunks it makes of `regionIds`.
    pub chunk_regions: u32,
    pub chunks: u32,
    /// Regions in scope left out: an earlier cloud request for each is still
    /// unresolved (unknown, accepted, or waiting to attach).
    pub unresolved_ids: Vec<String>,
    /// Regions in scope left out: the crop each needs is past the render
    /// service's limits even with no page around it
    /// ([`cleaner_core::engines::render::within_service_limits`]), so the
    /// service would refuse it. They stay detected, for a local clean.
    pub too_large_ids: Vec<String>,
    /// GPU seconds, `None` with nothing to send.
    pub estimated_gpu_seconds: Option<CostRange>,
    /// `None` when the gateway states no GPU price.
    pub estimated_cost_usd: Option<CostRange>,
    /// The deployed GPU, as the gateway names it.
    pub gpu: Option<String>,
    pub provider: CloudProvider,
    pub profile_name: String,
    /// The digest the consent is given for ([`compute_clean_plan_digest`]).
    pub plan_digest: Option<String>,
    pub expires_at_ms: u64,
    /// The chapter's project already consented to this endpoint, so the
    /// question is skipped. The plan digest and its grant are not.
    pub standing: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCleanGrant {
    pub grant_id: String,
    pub expires_at_ms: u64,
}

/// One region a clean grant covers, and the revision it covers it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrantedRegion {
    pub id: String,
    pub page_index: u32,
    pub order: u32,
    /// The mask's box, in the pixels of `space`. Orders renders.
    pub bbox: Rect,
    /// Which regions this one can read under ([`next_ready`]): its page's
    /// index on a paginated chapter, where `bbox` is in page pixels, and `0`
    /// for every region of a long strip, where `bbox` is in strip pixels
    /// because a render reads across the joins.
    pub space: u32,
    /// The crop the render will send, as the plan's recipe's preprocessing
    /// sizes it from the stored region ([`CloudRegion::estimated_crop`]).
    /// Sizes the estimate, and through it the plan digest.
    pub crop: Rect,
    pub revision_hash: String,
    /// The text group the region cleans, for provenance.
    pub group_id: Option<String>,
    /// A `fill` or `solid` pick: what mixed execution tries here first.
    pub local_candidate: bool,
}

/// A plan's estimate, from its jobs' own crops.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Estimate {
    pub crop_pixels: u64,
    pub work_pixels: u64,
    pub gpu_seconds: f64,
    pub cold_start_seconds: f64,
    pub retry_gpu_seconds: f64,
}

impl Estimate {
    fn of(regions: &[GrantedRegion], rate: RenderRate) -> Self {
        let empty = Self { cold_start_seconds: rate.cold_start_seconds, ..Self::default() };
        regions.iter().fold(empty, |mut total, region| {
            let crop = region.crop;
            let (w, h) = rate.working_size(crop.w, crop.h);
            let work = u64::from(w) * u64::from(h);
            total.crop_pixels += u64::from(crop.w) * u64::from(crop.h);
            total.work_pixels += work;
            let render_seconds = work as f64 / 1e6 * rate.seconds_per_work_megapixel;
            total.gpu_seconds += JOB_OVERHEAD_SECONDS + render_seconds;
            // Qwen can try two more seeds. High includes all three renders.
            if rate.qwen { total.retry_gpu_seconds += 2.0 * render_seconds; }
            total
        })
    }

    fn seconds(&self) -> CostRange {
        CostRange { low: self.gpu_seconds, high: self.gpu_seconds + self.retry_gpu_seconds + self.cold_start_seconds + IDLE_WINDOW_SECONDS }
    }
}

/// What a clean grant is bound to. Every field but the regions must match the
/// run that spends it; the regions are checked chunk by chunk and one by one
/// as the run reaches them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CleanGrantScope {
    pub chapter_id: String,
    /// In the order the run takes them.
    pub regions: Vec<GrantedRegion>,
    pub provider: CloudProvider,
    pub profile_id: String,
    pub endpoint_fingerprint: String,
    pub profile_epoch: u64,
    pub recipe: RenderRecipe,
    pub execution: CleanExecution,
    pub chunk_regions: usize,
    pub estimate: Estimate,
    /// The GPU and its list price the estimate used, as the gateway stated them.
    pub price: Option<(String, f64)>,
    pub cost: Option<CostRange>,
    pub plan_digest: String,
}

impl CleanGrantScope {
    fn terms(&self) -> CleanPlanTerms<'_> {
        let micro = |usd: f64| (usd * 1e6).round() as u64;
        CleanPlanTerms {
            chapter_id: &self.chapter_id,
            provider: self.provider,
            profile_id: &self.profile_id,
            endpoint_fingerprint: &self.endpoint_fingerprint,
            recipe: &self.recipe,
            execution: self.execution.key(),
            chunk_regions: self.chunk_regions as u32,
            regions: self.regions.iter().map(|region| PlanRegionTerms {
                id: &region.id,
                page_index: region.page_index,
                revision_hash: &region.revision_hash,
                group_id: region.group_id.as_deref(),
            }).collect(),
            estimate: PlanEstimateTerms {
                crop_pixels: self.estimate.crop_pixels,
                work_pixels: self.estimate.work_pixels,
                gpu_ms_low: (self.estimate.seconds().low * 1000.0).round() as u64,
                gpu_ms_high: (self.estimate.seconds().high * 1000.0).round() as u64,
                cost_micro_usd_low: self.cost.map(|cost| micro(cost.low)),
                cost_micro_usd_high: self.cost.map(|cost| micro(cost.high)),
                gpu: self.price.as_ref().map(|(gpu, _)| gpu.as_str()),
                gpu_micro_usd_per_hour: self.price.as_ref().map(|(_, price)| micro(*price)),
            },
        }
    }

    /// The digest of the plan as this scope holds it.
    pub(crate) fn digest(&self) -> Result<String, String> {
        compute_clean_plan_digest(&self.terms()).map_err(|e| e.to_string())
    }

    fn ordered_ids(&self) -> Vec<&str> {
        self.regions.iter().map(|region| region.id.as_str()).collect()
    }
}

/// What a starting run presents: the destination and model as they are now.
pub(crate) struct CleanGrantRequest<'a> {
    pub provider: CloudProvider,
    pub profile_id: &'a str,
    pub endpoint_fingerprint: &'a str,
    pub profile_epoch: u64,
    /// The deployment's recipe.
    pub recipe: &'a RenderRecipe,
}

/// A prepared plan awaiting confirmation. A prepare takes the job's lock (which
/// can wait on another process) only before it touches this cache, and holds
/// the cache's lock only for bookkeeping, so every other proposal command goes
/// on while one prepare waits on its chapter.
struct StoredProposal {
    expires_at_ms: u64,
    scope: CleanGrantScope,
}

struct StoredGrant {
    scope: CleanGrantScope,
    expires_at_ms: u64,
}

fn proposals() -> &'static Mutex<HashMap<String, StoredProposal>> {
    static PROPOSALS: OnceLock<Mutex<HashMap<String, StoredProposal>>> = OnceLock::new();
    PROPOSALS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn grants() -> &'static Mutex<HashMap<String, StoredGrant>> {
    static GRANTS: OnceLock<Mutex<HashMap<String, StoredGrant>>> = OnceLock::new();
    GRANTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn grant_has_capacity(store: &HashMap<String, StoredGrant>) -> bool {
    store.len() < MAX_OPEN_GRANTS
}

/// What [`prepare`] reads from the gateway. Neither call carries page data.
pub(crate) trait CleanGateway {
    /// The render recipe the deployment serves now.
    fn recipe(&self) -> Result<RenderRecipe, String>;
    /// The deployed GPU and its list price per hour, when the gateway states
    /// one. `None` makes the estimate `null`, never a guess.
    fn gpu_price(&self) -> Option<(String, f64)>;
}

/// A deployment prices the one GPU it runs. More than one entry, or a gateway
/// that predates the route, states no price for this worker.
fn only_price(status: GpuStatusWire) -> Option<(String, f64)> {
    let mut prices = status.list_price_usd_per_hour.into_iter();
    let only = prices.next()?;
    if prices.next().is_some() { return None; }
    Some(only).filter(|(_, price)| price.is_finite() && *price > 0.0)
}

impl CleanGateway for CloudHttpClient {
    fn recipe(&self) -> Result<RenderRecipe, String> {
        let info = self.get_model_info().map_err(|e| gpu::transport_code(&e))?;
        Ok(RenderRecipe::new(info.recipe_id, info.preprocessing_version, info.model_id,
            info.model_revision, info.native_mask_conditioning))
    }

    fn gpu_price(&self) -> Option<(String, f64)> {
        self.get_gpu_status().ok().flatten().and_then(only_price)
    }
}

/// What the caller asked to clean.
pub(crate) struct PrepareRequest {
    pub chapter_id: String,
    pub scope: String,
    pub page_indices: Option<Vec<u32>>,
    pub region_ids: Option<Vec<String>>,
    /// The explicit mixed choice ([`CleanExecution::Mixed`]). Off by default.
    pub local_first: bool,
    /// This clean's picks and the ceiling they start under, written onto the
    /// detections in scope before they are planned
    /// ([`crate::run::repick_detections`]). Text cleanup sends both sides.
    /// The region menu sends none, or under the mixed choice only a Fill or
    /// Solid bubble pick; a side not sent keeps each region's saved pick.
    pub picks: Option<run::Repick>,
    pub qwen_edit: Option<cleaner_core::engines::render::QwenEdit>,
}

/// Where the batch would go: the selected profile.
pub(crate) struct Destination {
    pub provider: CloudProvider,
    pub profile_id: String,
    pub profile_name: String,
    pub endpoint_fingerprint: String,
}

/// Whether an earlier cloud request for the region `id` on page `page` is
/// still unresolved. The app asks the attempt journal
/// ([`crate::inference::journal::AttemptJournal::unresolved_region_attempt`]).
pub(crate) type Unresolved<'a> = dyn Fn(u32, &str) -> bool + Sync + 'a;

/// State what a cloud clean would send. Nothing is cleaned, written or sent.
pub(crate) fn prepare<G: CleanGateway + ?Sized>(
    job_path: &Path,
    request: PrepareRequest,
    destination: Destination,
    gateway: &G,
    unresolved: &Unresolved<'_>,
) -> Result<CloudCleanProposal, String> {
    prepare_with_cache(job_path, request, destination, gateway, unresolved, proposals())
}

fn prepare_with_cache<G: CleanGateway + ?Sized>(
    job_path: &Path,
    request: PrepareRequest,
    destination: Destination,
    gateway: &G,
    unresolved: &Unresolved<'_>,
    proposals: &Mutex<HashMap<String, StoredProposal>>,
) -> Result<CloudCleanProposal, String> {
    let in_scope = {
        let _lock = run::lock_job(job_path)?;
        let job = Job::open(job_path).map_err(|e| e.to_string())?;
        detections_in_scope(&job, &request)?
    };
    if in_scope.len() > MAX_PLAN_REGIONS { return Err("cloud_clean_too_many_regions".into()); }
    // Work whose outcome is still unknown is not planned again: a second
    // request could render and bill the same region twice.
    let (pending, held): (Vec<_>, Vec<_>) =
        in_scope.into_iter().partition(|(page, id)| !unresolved(*page, id));
    let open = |cache: &mut HashMap<String, StoredProposal>| {
        cache.retain(|_, stored| stored.expires_at_ms > analysis::now_ms());
        if cache.len() >= MAX_OPEN_PROPOSALS { Err("cloud_clean_proposal_limit".to_string()) } else { Ok(()) }
    };
    let ids: Vec<String> = pending.into_iter().map(|(_, id)| id).collect();
    // The clean's own picks decide where each region is cleaned, so they are
    // saved on it first. A held region is left as it is: its revision is what
    // its unresolved request will be matched against.
    if let Some(repick) = request.picks {
        run::repick_detections(job_path, &ids, repick)?;
    }
    // Each job's crop is the one the deployment's recipe will cut: its
    // preprocessing version sizes it, so the recipe is read before the
    // regions are bound. Nothing in scope asks the gateway nothing.
    let (recipe, Bound { regions, too_large }) = if ids.is_empty() {
        (None, Bound::default())
    } else {
        open(&mut *proposals.lock().map_err(|e| e.to_string())?)?;
        let mut recipe = gateway.recipe()?;
        if recipe.model_id.starts_with(QWEN_MODEL_PREFIX) {
            if recipe.recipe_id != "mc-qwen-image-edit-2511-v4" { return Err("qwen_update_required".into()); }
            recipe.qwen_edit = Some(request.qwen_edit.clone().unwrap_or(cleaner_core::engines::render::QwenEdit {
                target: cleaner_core::engines::render::QwenTarget::Auto, description: String::new(),
            }));
        } else if request.qwen_edit.is_some() { return Err("cloud_clean_recipe_changed".into()); }
        cleaner_core::cloud_wire::validate_recipe(&recipe.clone().into()).map_err(|_| "invalid_request".to_string())?;
        let bound = bind(job_path, &ids, recipe_preprocessing(&recipe)?)?;
        (Some(recipe), bound)
    };
    let execution = if request.local_first { CleanExecution::Mixed } else { CleanExecution::Cloud };
    let page_indices: Vec<u32> = regions.iter().map(|region| region.page_index)
        .collect::<BTreeSet<_>>().into_iter().collect();
    let estimate = recipe.as_ref().map_or_else(Estimate::default, |recipe| Estimate::of(&regions, RenderRate::of(recipe)));
    let mut proposal = CloudCleanProposal {
        proposal_id: None,
        chapter_id: request.chapter_id,
        region_ids: regions.iter().map(|region| region.id.clone()).collect(),
        regions: regions.len() as u32,
        pages: page_indices.len() as u32,
        page_indices,
        total_crop_pixels: estimate.crop_pixels,
        total_work_pixels: estimate.work_pixels,
        local_cleaned: 0,
        execution,
        local_candidates: if execution == CleanExecution::Mixed {
            regions.iter().filter(|region| region.local_candidate).count() as u32
        } else {
            0
        },
        chunk_regions: CHUNK_REGIONS as u32,
        chunks: chunk_count(regions.len(), CHUNK_REGIONS) as u32,
        unresolved_ids: held.into_iter().map(|(_, id)| id).collect(),
        too_large_ids: too_large,
        estimated_gpu_seconds: None,
        estimated_cost_usd: None,
        gpu: None,
        provider: destination.provider,
        profile_name: destination.profile_name,
        plan_digest: None,
        expires_at_ms: 0,
        standing: false,
    };
    let Some(recipe) = recipe.filter(|_| !regions.is_empty()) else { return Ok(proposal) };
    let price = gateway.gpu_price();
    // Recheck after the network reads: another prepare could have taken the
    // last slot in the meantime.
    let mut cache = proposals.lock().map_err(|e| e.to_string())?;
    open(&mut cache)?;
    let proposal_id = analysis::random_id()?;
    let cost = price.as_ref().map(|(_, per_hour)| cost(estimate.gpu_seconds, *per_hour, &recipe, destination.provider));
    let mut scope = CleanGrantScope {
        chapter_id: proposal.chapter_id.clone(),
        regions,
        provider: destination.provider,
        profile_epoch: GrantService::global().get_profile_epoch(destination.provider, &destination.profile_id),
        profile_id: destination.profile_id,
        endpoint_fingerprint: destination.endpoint_fingerprint,
        recipe,
        execution,
        chunk_regions: CHUNK_REGIONS,
        estimate,
        price,
        cost,
        plan_digest: String::new(),
    };
    scope.plan_digest = scope.digest()?;
    proposal.estimated_gpu_seconds = Some(estimate.seconds());
    proposal.estimated_cost_usd = cost;
    proposal.gpu = scope.price.as_ref().map(|(gpu, _)| gpu.clone());
    proposal.plan_digest = Some(scope.plan_digest.clone());
    proposal.proposal_id = Some(proposal_id.clone());
    proposal.expires_at_ms = analysis::now_ms().saturating_add(
        crate::inference::consent::DEFAULT_PROPOSAL_TTL.as_millis() as u64);
    cache.insert(proposal_id, StoredProposal { expires_at_ms: proposal.expires_at_ms, scope });
    Ok(proposal)
}

/// The preprocessing a recipe names, which sizes every crop a plan under it
/// sends. A version this build cannot prepare is refused before anything is
/// planned: its renders would be refused one by one.
fn recipe_preprocessing(recipe: &RenderRecipe) -> Result<Preprocessing, String> {
    Preprocessing::of(&recipe.preprocessing_version)
        .map_err(|_| "cloud_clean_unsupported_preprocessing".to_string())
}

/// The detections a scope names, with the page each is on, in page order and
/// then reading order. A detection whose page has left the chapter is not in
/// any scope.
fn detections_in_scope(job: &Job, request: &PrepareRequest) -> Result<Vec<(u32, String)>, String> {
    let rows = &job.project.detections;
    let chosen: Vec<&cleaner_core::project::DetectedRegion> = match request.scope.as_str() {
        "page" => {
            let pages = request.page_indices.as_deref().filter(|pages| !pages.is_empty())
                .ok_or("cloud_clean_no_pages")?;
            let sources = pages.iter()
                .map(|page| Library::resolve_page(&job.project, *page as usize).ok_or("cloud_clean_page_not_found"))
                .collect::<Result<HashSet<usize>, _>>()?;
            rows.iter().filter(|row| sources.contains(&row.source_idx)).collect()
        }
        "chapter" => rows.iter().collect(),
        "regions" => {
            let ids = request.region_ids.as_deref().filter(|ids| !ids.is_empty())
                .ok_or("cloud_clean_no_regions")?;
            let mut by_id: HashMap<&str, &cleaner_core::project::DetectedRegion> = HashMap::new();
            for row in rows { by_id.entry(row.id.as_str()).or_insert(row); }
            ids.iter().map(|id| by_id.get(id.as_str()).copied().ok_or("cloud_clean_region_not_found"))
                .collect::<Result<_, _>>()?
        }
        _ => return Err("cloud_clean_scope_unsupported".into()),
    };
    let mut placements = HashMap::new();
    for (page, source) in job.project.strip.order.iter().enumerate() {
        placements.entry(*source).or_insert(page);
    }
    let mut ordered: Vec<(usize, u32, &str)> = chosen.into_iter().filter_map(|row| {
        let placement = *placements.get(&row.source_idx)?;
        Some((placement, row.order, row.id.as_str()))
    }).collect();
    ordered.sort_unstable();
    ordered.dedup();
    Ok(ordered.into_iter().map(|(page, _, id)| (page as u32, id.to_owned())).collect())
}

/// Each detection's binding as the run will re-check it, through the same
/// reading the render makes ([`load_cloud_region`]), with the crop a render
/// under `preprocessing` sends from its page. An id no detection carries any
/// more (cleaned or deleted in between) is left out; one whose crop the
/// render service would refuse is answered apart, the second list, and never
/// reaches a plan, a price or a consent.
fn bind(job_path: &Path, ids: &[String], preprocessing: Preprocessing) -> Result<Bound, String> {
    let _lock = run::lock_job(job_path)?;
    let job = Job::open(job_path).map_err(|e| e.to_string())?;
    let mut by_id = HashMap::new();
    for row in &job.project.detections { by_id.entry(row.id.as_str()).or_insert(row); }
    let mut placements = HashMap::new();
    for (page, source) in job.project.strip.order.iter().enumerate() {
        placements.entry(*source).or_insert(page);
    }
    let mut sources: HashMap<usize, String> = HashMap::new();
    let strip = (job.project.strip.mode == StripMode::Longstrip).then(|| run::strip_of(&job.project));
    let mut bound = Bound { regions: Vec::with_capacity(ids.len()), ..Bound::default() };
    for id in ids {
        let Some(row) = by_id.get(id.as_str()).copied() else { continue };
        let Some(placement) = placements.get(&row.source_idx).copied()
            else { continue };
        let source_sha256 = match sources.get(&row.source_idx) {
            Some(sha) => sha.clone(),
            None => {
                let path = job.source_path(row.source_idx).ok_or("cloud_clean_source_missing")?;
                let sha = sha256_hex(&std::fs::read(path).map_err(|e| e.to_string())?);
                sources.insert(row.source_idx, sha.clone());
                sha
            }
        };
        let region = load_cloud_region(&job, id, &source_sha256).map_err(|e| e.to_string())?;
        let (group_id, local_candidate) = match &region.kind {
            CloudRegionKind::Detection(record) => (
                record.group.as_ref().map(|group| group.id.clone()),
                matches!(record.pick.as_str(), "fill" | "solid"),
            ),
            CloudRegionKind::Patch(_) => (None, false),
        };
        let page = job.project.sources.get(row.source_idx).ok_or("cloud_clean_source_missing")?;
        let Some(crop) = region.sendable_crop(preprocessing, page.w, page.h) else {
            bound.too_large.push(id.clone());
            continue;
        };
        let (space, bbox) = match &strip {
            Some(strip) => {
                let origin = strip.pages().get(placement).map_or((0, 0), |page| (page.x_offset, page.y_offset));
                let bounds = region.mask.bounds;
                (0, Rect::new(bounds.x + origin.0, bounds.y + origin.1, bounds.w, bounds.h))
            }
            None => (placement as u32, region.mask.bounds),
        };
        bound.regions.push(GrantedRegion {
            id: id.clone(),
            page_index: placement as u32,
            order: region.order,
            bbox,
            space,
            revision_hash: region.revision_hash,
            crop,
            group_id,
            local_candidate,
        });
    }
    Ok(bound)
}

/// What [`bind`] makes of a scope's ids, each list in page and reading order.
#[derive(Debug, Default)]
struct Bound {
    /// For the cloud.
    regions: Vec<GrantedRegion>,
    /// Past the render service's limits.
    too_large: Vec<String>,
}

/// The size the worker edits a `w` by `h` crop at: upscaled so its long side
/// is [`WORK_LONG_SIDE`] and snapped down to the latent stride, never
/// downscaled (`working_size`, deploy/cloud/common/flux.py).
fn working_size(w: u32, h: u32) -> (u32, u32) {
    let long = w.max(h).max(1);
    if long >= WORK_LONG_SIDE { return (w, h); }
    let scale = f64::from(WORK_LONG_SIDE) / f64::from(long);
    let snap = |extent: u32| {
        let scaled = (f64::from(extent) * scale).round() as u32;
        (scaled - scaled % LATENT_STRIDE).max(LATENT_STRIDE)
    };
    (snap(w), snap(h))
}

/// The cost range of `gpu_seconds` of rendering at `gpu_usd_per_hour`.
fn cost(gpu_seconds: f64, gpu_usd_per_hour: f64, recipe: &RenderRecipe, provider: CloudProvider) -> CostRange {
    // The worker's memory follows the model and the provider (`memory=` on
    // the render class).
    let rate = RenderRate::of(recipe);
    let per_second = gpu_usd_per_hour / 3600.0 + CPU_CORES * CPU_USD_PER_CORE_SECOND
        + rate.memory_gib(provider) * MEMORY_USD_PER_GIB_SECOND;
    CostRange {
        low: gpu_seconds * per_second,
        high: (gpu_seconds + rate.cold_start_seconds + IDLE_WINDOW_SECONDS) * per_second,
    }
}

/// Turn a proposal the user confirmed into a clean grant. `plan_digest` is
/// the plan the consent dialog showed: the grant is minted only when it is
/// the plan this proposal holds, so what consent was given for is the plan,
/// not only the proposal's id. Missing consent leaves the proposal open; any
/// other answer, a mismatched plan included, spends it.
pub(crate) fn confirm(proposal_id: &str, plan_digest: &str, rights_attested: bool, retention_acknowledged: bool)
    -> Result<CloudCleanGrant, String> {
    analysis::require_consent(rights_attested, retention_acknowledged).map_err(|e| e.to_string())?;
    let stored = proposals().lock().map_err(|e| e.to_string())?
        .remove(proposal_id).ok_or("cloud_clean_proposal_missing")?;
    let now = analysis::now_ms();
    if now >= stored.expires_at_ms { return Err("cloud_clean_proposal_expired".into()); }
    let scope = stored.scope;
    if plan_digest != scope.plan_digest { return Err("cloud_clean_plan_mismatch".into()); }
    if GrantService::global().get_profile_epoch(scope.provider, &scope.profile_id) != scope.profile_epoch {
        return Err("cloud_clean_profile_changed".into());
    }
    let grant = CloudCleanGrant {
        grant_id: analysis::random_id()?,
        expires_at_ms: now.saturating_add(CLEAN_GRANT_TTL.as_millis() as u64),
    };
    let mut store = grants().lock().map_err(|e| e.to_string())?;
    store.retain(|_, entry| entry.expires_at_ms > now);
    if !grant_has_capacity(&store) { return Err("cloud_clean_grant_limit".into()); }
    store.insert(grant.grant_id.clone(), StoredGrant { scope, expires_at_ms: grant.expires_at_ms });
    Ok(grant)
}

/// The chapter an open grant cleans, read without spending it.
pub(crate) fn grant_chapter(grant_id: &str) -> Option<String> {
    grants().lock().ok()?.get(grant_id).map(|stored| stored.scope.chapter_id.clone())
}

/// The provider and profile an open grant cleans on, read without spending it.
fn grant_profile(grant_id: &str) -> Option<(CloudProvider, String)> {
    grants().lock().ok()?.get(grant_id).map(|stored| (stored.scope.provider, stored.scope.profile_id.clone()))
}

/// Where an open proposal would send its regions: chapter, provider, profile
/// and endpoint fingerprint, for the project's standing consent.
pub(crate) fn proposal_destination(proposal_id: &str) -> Option<(String, CloudProvider, String, String)> {
    let cache = proposals().lock().ok()?;
    let scope = &cache.get(proposal_id)?.scope;
    Some((scope.chapter_id.clone(), scope.provider, scope.profile_id.clone(), scope.endpoint_fingerprint.clone()))
}

/// [`confirm`] for a proposal in `library`'s projects. A plan the project's
/// standing consent covers is confirmed with the statements that consent was
/// given with, so the interface never answers them for the user. Only an
/// answer that ticked both is recorded to stand.
pub(crate) fn confirm_in_project(library: Option<&Library>, proposal_id: &str, plan_digest: &str,
    rights_attested: bool, retention_acknowledged: bool) -> Result<CloudCleanGrant, String> {
    let destination = proposal_destination(proposal_id);
    let standing = match (&destination, library) {
        (Some((chapter_id, provider, profile_id, endpoint)), Some(library)) =>
            library.cloud_consent_covers(chapter_id, *provider, profile_id, endpoint),
        _ => false,
    };
    let grant = confirm(proposal_id, plan_digest, rights_attested || standing, retention_acknowledged || standing)?;
    // The first consent in a project stands for the rest of it.
    if let (Some((chapter_id, provider, profile_id, endpoint)), Some(library)) = (destination, library) {
        library.record_cloud_consent(&chapter_id, provider, &profile_id, &endpoint,
            rights_attested && retention_acknowledged);
    }
    Ok(grant)
}

/// Throw an unconfirmed proposal away. `false` when there was none.
pub(crate) fn discard(proposal_id: &str) -> Result<bool, String> {
    Ok(proposals().lock().map_err(|e| e.to_string())?.remove(proposal_id).is_some())
}

/// Spend a clean grant. It is gone after the first presentation, matched or
/// not, so a refused grant cannot be tried again with other arguments.
pub(crate) fn consume_grant(grant_id: &str, request: &CleanGrantRequest<'_>) -> Result<CleanGrantScope, String> {
    let stored = grants().lock().map_err(|e| e.to_string())?
        .remove(grant_id).ok_or("cloud_clean_grant_missing")?;
    if analysis::now_ms() >= stored.expires_at_ms { return Err("cloud_clean_grant_expired".into()); }
    let scope = stored.scope;
    if scope.provider != request.provider || scope.profile_id != request.profile_id {
        return Err("cloud_clean_grant_mismatch: profile".into());
    }
    if scope.endpoint_fingerprint != request.endpoint_fingerprint {
        return Err("cloud_clean_grant_mismatch: endpoint".into());
    }
    if !scope.recipe.same_deployment(request.recipe) {
        return Err("cloud_clean_grant_mismatch: recipe".into());
    }
    if scope.profile_epoch != request.profile_epoch { return Err("cloud_clean_profile_changed".into()); }
    Ok(scope)
}

/* ------------------------------------------------------------------ */
/* The run                                                             */
/* ------------------------------------------------------------------ */

/// The render behind the run, so the run can be driven without a gateway.
pub(crate) trait RegionRenderer: Sync {
    /// Render one region with the region grant `nonce` and commit it. `Err` is
    /// the stable code it failed with ([`InferenceServiceError::code`]).
    fn render(&self, nonce: &str, page_index: u32, region_id: &str, recipe: &RenderRecipe) -> Result<ApiRegion, String>;

    /// The deployment as it is now: its recipe, and its GPU and list price
    /// read exactly as [`prepare`] reads them ([`CleanGateway::gpu_price`]: a
    /// price not stated, or not readable, is `None`). Asked before every
    /// chunk; `Err` is the stop code of a recipe that could not be read.
    fn deployment(&self) -> Result<(RenderRecipe, Option<(String, f64)>), String>;

    /// Ask the render under `attempt_id` to stop. Asked again on every tick
    /// until the run ends, so a render not yet registered when first asked
    /// is still reached.
    fn cancel(&self, attempt_id: &str) {
        request_render_cancel(attempt_id);
    }

    /// Release the render GPU if it is idle. Best effort.
    fn release_idle(&self);
}

/// A local pass: clean these ids here, and answer the ids it cleaned. Mixed
/// execution's tries a fill or a solid colour.
pub(crate) type LocalPass<'a> = dyn Fn(&[String]) -> Result<Vec<String>, String> + Sync + 'a;

/// How the run reads a region again before its grant is minted: in the app,
/// always [`read_render_input`], from disk under the job's lock.
pub(crate) type ReadInput<'a> =
    dyn Fn(&Path, u32, &str, &RenderRecipe) -> Result<RenderInput, InferenceServiceError> + Sync + 'a;

/// What a spent grant lets a run do, and against what.
pub(crate) struct Batch<'a> {
    pub scope: &'a CleanGrantScope,
    pub job_path: &'a Path,
    /// Where region grants are minted: the one the render spends them from.
    pub grants: &'a GrantService,
    /// Whether the run still speaks for the endpoint: `Err` is the code that
    /// stops it. Asked before every chunk and every region.
    pub allowed: &'a (dyn Fn() -> Result<(), &'static str> + Sync),
    /// Asked of every region as its chunk is minted.
    pub unresolved: &'a Unresolved<'a>,
    /// Run only under [`CleanExecution::Mixed`].
    pub local_pass: Option<&'a LocalPass<'a>>,
    pub read: &'a ReadInput<'a>,
}

impl Batch<'_> {
    /// The run's authority is gone: every later region would fail the same way.
    fn lost_authority(&self) -> bool {
        (self.allowed)().is_err()
            || self.grants.get_profile_epoch(self.scope.provider, &self.scope.profile_id) != self.scope.profile_epoch
    }

    /// Whether the next chunk may start: the run still speaks for the
    /// endpoint, and the deployment is the one the plan was priced against.
    fn revalidate(&self, renderer: &dyn RegionRenderer) -> Result<(), String> {
        (self.allowed)().map_err(str::to_owned)?;
        if self.grants.get_profile_epoch(self.scope.provider, &self.scope.profile_id) != self.scope.profile_epoch {
            return Err("cloud_run_profile_changed".into());
        }
        let (recipe, price) = renderer.deployment()?;
        if !recipe.same_deployment(&self.scope.recipe) { return Err("cloud_clean_recipe_changed".into()); }
        // The price state is part of consent even when it was unknown. If the
        // gateway later states a price, the shown plan no longer describes it.
        if price != self.scope.price {
            return Err("cloud_clean_gpu_changed".into());
        }
        Ok(())
    }
}

/// Where one region ended.
enum Outcome {
    Cleaned(Box<ApiRegion>),
    /// `changed` or `gone`: not what consent covered.
    Skipped(&'static str),
    Failed(String),
    /// Stopped by the run's end before a result: left as it was, no notice.
    Left,
}

enum Message {
    Started(usize),
    Finished {
        index: usize,
        outcome: Outcome,
        /// The region reached the renderer.
        reached: bool,
        /// Its failure withdrew the run's authority.
        stops: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// In a chunk not yet minted.
    Waiting,
    Queued,
    InFlight,
    Done,
}

struct Schedule {
    slots: Vec<Slot>,
    /// No region is started after this.
    stop: bool,
    /// The attempt each region in flight renders under.
    attempts: HashMap<usize, String>,
}

struct Shared {
    schedule: Mutex<Schedule>,
    changed: Condvar,
}

impl Shared {
    /// Poison is recovered: the schedule is plain data, and a worker that
    /// panicked has already given up its region.
    fn lock(&self) -> MutexGuard<'_, Schedule> {
        self.schedule.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Start nothing more, and cancel what is in flight.
    fn halt(&self, renderer: &dyn RegionRenderer) {
        let attempts: Vec<String> = {
            let mut schedule = self.lock();
            schedule.stop = true;
            schedule.attempts.values().cloned().collect()
        };
        self.changed.notify_all();
        for attempt in attempts {
            renderer.cancel(&attempt);
        }
    }
}

/// The first queued region nothing still to be committed reads under: no
/// region below it in its space (its page, or the whole long strip) and within
/// [`DEPENDENCY_REACH`] is queued or in flight. "Below" is the page order a
/// render composites by ([`crate::underlay`]). On a page that is every such
/// region just ahead of it in the plan; on a long strip, where orders are
/// counted per page, one on the next page can be below it too, so every
/// region of its chunk is asked. A region of a later chunk is not waited on:
/// that chunk is not minted until this one ends.
fn next_ready(slots: &[Slot], regions: &[GrantedRegion]) -> Option<usize> {
    let active: Vec<usize> = (0..slots.len())
        .filter(|&index| matches!(slots[index], Slot::Queued | Slot::InFlight))
        .collect();
    active.iter().copied().find(|&index| {
        slots[index] == Slot::Queued && !active.iter().any(|&below| {
            below != index
                && regions[below].space == regions[index].space
                && regions[below].order < regions[index].order
                && within_reach(regions[below].bbox, regions[index].bbox)
        })
    })
}

fn within_reach(below: Rect, above: Rect) -> bool {
    below.x < above.right() + DEPENDENCY_REACH
        && above.x < below.right() + DEPENDENCY_REACH
        && below.y < above.bottom() + DEPENDENCY_REACH
        && above.y < below.bottom() + DEPENDENCY_REACH
}

/// The run's account of pages and regions, across its chunks. Every event the
/// run emits goes through here, on the run's own thread.
struct Progress<'a> {
    run_id: &'a str,
    chapter_id: &'a str,
    job_path: &'a Path,
    emit: &'a dyn Fn(Event),
    /// Regions each page still has to settle.
    remaining: BTreeMap<u32, usize>,
    started: BTreeSet<u32>,
    cleaned_pages: BTreeSet<u32>,
    failed_pages: BTreeSet<u32>,
    /// Pages with a region the run's end left unrendered: where a resume starts.
    unfinished: BTreeSet<u32>,
    regions_cleaned: u32,
    /// Anything reached the renderer.
    sent: bool,
    /// The run stopped, for whatever reason, and said so once.
    stopped: bool,
    /// One of its stops withdrew the run's authority over the endpoint
    /// ([`withdraws_authority`]): it asks the endpoint nothing more, not even
    /// to release the GPU. A stop over a changed deployment or a refused
    /// chunk does not, so that GPU is still released.
    authority_lost: bool,
}

impl<'a> Progress<'a> {
    fn new(scope: &'a CleanGrantScope, job_path: &'a Path, run_id: &'a str, emit: &'a dyn Fn(Event)) -> Self {
        let mut remaining = BTreeMap::new();
        for page in scope.regions.iter().map(|region| region.page_index) {
            *remaining.entry(page).or_default() += 1;
        }
        Self {
            run_id, chapter_id: &scope.chapter_id, job_path, emit, remaining,
            started: BTreeSet::new(), cleaned_pages: BTreeSet::new(), failed_pages: BTreeSet::new(),
            unfinished: BTreeSet::new(), regions_cleaned: 0, sent: false, stopped: false, authority_lost: false,
        }
    }

    fn start(&mut self, page: u32) {
        if self.started.insert(page) {
            (self.emit)(run::page_started_event(self.run_id, self.chapter_id, page));
        }
    }

    /// A region of `page` is cleaned. Without the region as the chapter holds
    /// it (it could not be read back) it is still counted, and the page's
    /// `page-done` carries it.
    fn cleaned(&mut self, page: u32, region: Option<Box<ApiRegion>>) {
        self.regions_cleaned += 1;
        self.cleaned_pages.insert(page);
        let Some(region) = region else { return };
        (self.emit)(Event::RegionDone {
            run_id: self.run_id.to_owned(),
            chapter_id: self.chapter_id.to_owned(),
            page_id: library::page_id(self.chapter_id, page as usize),
            page_index: page,
            region,
        });
    }

    fn skipped(&self, region_id: &str, page: u32, reason: &str) {
        (self.emit)(events::notice_event(
            "notice.cloudClean.regionSkipped",
            serde_json::json!({ "regionId": region_id, "page": page + 1, "reason": reason }),
            "warn",
        ));
    }

    fn failed(&mut self, region: &GrantedRegion, code: &str) {
        self.failed_pages.insert(region.page_index);
        (self.emit)(events::notice_event(
            "notice.cloudClean.regionFailed",
            serde_json::json!({ "regionId": region.id, "page": region.page_index + 1, "code": code }),
            "warn",
        ));
    }

    /// The first stop says so for the run; the ones after it say nothing more.
    fn stop(&mut self, page: u32, code: &str) {
        self.authority_lost |= withdraws_authority(code);
        if !self.stopped {
            self.stopped = true;
            (self.emit)(events::notice_event(
                "notice.cloudClean.stopped",
                serde_json::json!({ "page": page + 1, "code": code }),
                "warn",
            ));
        }
    }

    /// One region of `page` is settled, whichever way.
    fn settle(&mut self, page: u32) {
        if let Some(left) = self.remaining.get_mut(&page) {
            *left -= 1;
            if *left == 0 && self.started.contains(&page) {
                (self.emit)(run::page_done_after_clean(self.run_id, self.chapter_id, self.job_path, page));
            }
        }
    }

    fn finish(mut self, cancelled: bool) -> Summary {
        // A page the run started and did not finish still ends, so its mark clears.
        for (&page, &left) in &self.remaining {
            if left > 0 {
                self.unfinished.insert(page);
                if self.started.contains(&page) {
                    (self.emit)(run::page_done_after_clean(self.run_id, self.chapter_id, self.job_path, page));
                }
            }
        }
        Summary {
            reason: if cancelled { "cancelled" } else { "completed" },
            pages_queued: self.remaining.len() as u32,
            pages_cleaned: self.cleaned_pages.len() as u32,
            regions_cleaned: self.regions_cleaned,
            next_page_index: self.unfinished.first().copied().filter(|_| cancelled),
            errored: self.failed_pages.len() as u32,
            regions_detected: 0,
        }
    }
}

/// Run a spent grant's plan, chunk by chunk. The caller's thread emits every
/// event; the renders run on [`MAX_IN_FLIGHT`] workers of their own.
pub(crate) fn run_plan(
    batch: &Batch<'_>,
    renderer: &dyn RegionRenderer,
    run_id: &str,
    cancel: &AtomicBool,
    emit: &dyn Fn(Event),
) -> Summary {
    let scope = batch.scope;
    let regions = &scope.regions;
    let ordered = scope.ordered_ids();
    let mut progress = Progress::new(scope, batch.job_path, run_id, emit);
    let mut slots = vec![Slot::Waiting; regions.len()];
    for index in 0..chunk_count(regions.len(), scope.chunk_regions) {
        if progress.stopped || cancel.load(Ordering::SeqCst) { break; }
        let Some(range) = chunk_range(regions.len(), scope.chunk_regions, index) else { break };
        if range.clone().all(|position| slots[position] == Slot::Done) { continue; }
        if let Err(code) = batch.revalidate(renderer) {
            progress.stop(regions[range.start].page_index, &code);
            break;
        }
        if scope.execution == CleanExecution::Mixed {
            if let Some(pass) = batch.local_pass {
                clean_here_first(batch, pass, range.clone(), &mut slots, &mut progress, cancel);
            }
            if cancel.load(Ordering::SeqCst) { break; }
        }
        let Some(grant) = issue_chunk_grant(&scope.plan_digest, &ordered, scope.chunk_regions, index,
            &mut |position| {
                let region = &regions[position];
                if slots[position] == Slot::Done { return false; }
                if (batch.unresolved)(region.page_index, &region.id) {
                    progress.skipped(&region.id, region.page_index, "unresolved");
                    slots[position] = Slot::Done;
                    progress.settle(region.page_index);
                    return false;
                }
                true
            }) else { break };
        if let Err(code) = run_chunk(batch, renderer, &grant, &mut slots, &mut progress, cancel) {
            progress.stop(regions[range.start].page_index, code);
            break;
        }
    }
    let cancelled = cancel.load(Ordering::SeqCst);
    // Whatever ended the plan, a GPU it woke is released, unless the run no
    // longer speaks for the endpoint.
    if progress.sent && !progress.authority_lost && !batch.lost_authority() {
        renderer.release_idle();
    }
    progress.finish(cancelled)
}

/// A stop after which the run no longer speaks for the endpoint: cloud engines
/// off, another or an edited profile, a refused or missing key.
fn withdraws_authority(code: &str) -> bool {
    matches!(code.split(':').next().unwrap_or("").trim(),
        "cloud_disabled" | "cloud_run_profile_changed" | "gateway_unauthorized" | "credential_missing")
}

/// Mixed execution's local pass over one chunk, just before the chunk goes
/// to the cloud: every `fill` or `solid` pick in it not yet settled whose
/// earlier cloud request is not unresolved. One page per call, so the job is
/// held for one page at a time and a cancel is heard before the next page.
/// What is cleaned here ([`pass_page`]) is final and leaves the plan; the
/// rest go to the cloud.
fn clean_here_first(batch: &Batch<'_>, pass: &LocalPass<'_>, range: std::ops::Range<usize>, slots: &mut [Slot],
    progress: &mut Progress<'_>, cancel: &AtomicBool) {
    let regions = &batch.scope.regions;
    let mut pages: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for position in range {
        let region = &regions[position];
        if slots[position] == Slot::Waiting && region.local_candidate
            && !(batch.unresolved)(region.page_index, &region.id) {
            pages.entry(region.page_index).or_default().push(position);
        }
    }
    for (page, positions) in pages {
        if cancel.load(Ordering::SeqCst) { return; }
        let ids: Vec<String> = positions.iter().map(|&position| regions[position].id.clone()).collect();
        let (mut cleaned, _) = pass_page(batch, pass, "mixed clean's local pass", page, &ids);
        for position in positions {
            let region = &regions[position];
            let Some(found) = cleaned.remove(&region.id) else { continue };
            slots[position] = Slot::Done;
            progress.start(region.page_index);
            progress.cleaned(region.page_index, found.map(Box::new));
            progress.settle(region.page_index);
        }
    }
}

/// Run `pass` over one page's `ids` and answer what it committed, and whether
/// it failed. What it committed is read back from the chapter rather than
/// taken from its answer: a call that failed partway has already saved the
/// regions it cleaned before the failure, and those count as cleaned too.
fn pass_page(batch: &Batch<'_>, pass: &LocalPass<'_>, what: &str, page: u32, ids: &[String])
    -> (HashMap<String, Option<ApiRegion>>, bool) {
    let answered = pass(ids);
    if let Err(error) = &answered {
        eprintln!("manga-cleaner: {what} on page {} stopped: {error}", page + 1);
        // What it saved before failing is on disk; the page reads must see it.
        library::invalidate_manifest_cache(batch.job_path);
    }
    let failed = answered.is_err();
    let cleaned = match committed_regions(batch.job_path, &batch.scope.chapter_id, page, ids) {
        Ok(found) => found.into_iter().map(|(id, region)| (id, Some(region))).collect(),
        Err(error) => {
            eprintln!("manga-cleaner: page {}'s local cleans could not be read back: {error}", page + 1);
            answered.unwrap_or_default().into_iter().map(|id| (id, None)).collect()
        }
    };
    (cleaned, failed)
}

/// The regions among `ids` that are patches of `page` now, as the chapter
/// holds them. A read, so without the job lock (see [`run::lock_job`]): a
/// writer holding the chapter does not stop the run from saying what it did.
fn committed_regions(job_path: &Path, chapter_id: &str, page: u32, ids: &[String])
    -> Result<HashMap<String, ApiRegion>, String> {
    let job = Job::open(job_path).map_err(|e| e.to_string())?;
    let patched: HashSet<&str> = job.project.patches.iter().map(|row| row.id.as_str()).collect();
    let wanted: HashSet<&str> = ids.iter().map(String::as_str).filter(|id| patched.contains(id)).collect();
    if wanted.is_empty() { return Ok(HashMap::new()); }
    let regions = library::page_of(chapter_id, &job.project, page as usize).ok_or("page_not_found")?.regions;
    Ok(regions.into_iter()
        .filter(|region| wanted.contains(region.id.as_str()))
        .map(|region| (region.id.clone(), region))
        .collect())
}

/// Run one chunk: prove its grant a subset of the plan, then render its
/// regions. `Err` is the stop code of a chunk refused before anything started.
fn run_chunk(
    batch: &Batch<'_>,
    renderer: &dyn RegionRenderer,
    grant: &ChunkGrant,
    slots: &mut Vec<Slot>,
    progress: &mut Progress<'_>,
    cancel: &AtomicBool,
) -> Result<(), &'static str> {
    let scope = batch.scope;
    let regions = &scope.regions;
    let positions = verify_chunk_grant(&scope.plan_digest, &scope.ordered_ids(), scope.chunk_regions,
        MAX_CLEAN_REGIONS, grant)?;
    if positions.is_empty() { return Ok(()); }
    for &position in &positions {
        slots[position] = Slot::Queued;
    }
    let shared = Shared {
        schedule: Mutex::new(Schedule { slots: std::mem::take(slots), stop: false, attempts: HashMap::new() }),
        changed: Condvar::new(),
    };
    let mut halting = progress.stopped;
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|threads| {
        for _ in 0..MAX_IN_FLIGHT.min(positions.len()) {
            let tx = tx.clone();
            let shared = &shared;
            threads.spawn(move || work(batch, renderer, shared, cancel, &tx));
        }
        drop(tx);
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Message::Started(index)) => progress.start(regions[index].page_index),
                Ok(Message::Finished { index, outcome, reached, stops }) => {
                    progress.sent |= reached;
                    let region = &regions[index];
                    match outcome {
                        Outcome::Cleaned(api) => progress.cleaned(region.page_index, Some(api)),
                        Outcome::Skipped(reason) => progress.skipped(&region.id, region.page_index, reason),
                        Outcome::Failed(code) if stops => {
                            // The ones in flight with it are cancelled and
                            // say nothing more.
                            progress.stop(region.page_index, &code);
                            halting = true;
                        }
                        Outcome::Failed(code) => progress.failed(region, &code),
                        Outcome::Left => {
                            progress.unfinished.insert(region.page_index);
                        }
                    }
                    progress.settle(region.page_index);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            halting |= cancel.load(Ordering::SeqCst);
            if halting {
                shared.halt(renderer);
            }
        }
    });
    *slots = shared.schedule.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner()).slots;
    Ok(())
}

/// One worker: the next ready region, until none is left or the run stops.
fn work(batch: &Batch<'_>, renderer: &dyn RegionRenderer, shared: &Shared, cancel: &AtomicBool,
    tx: &mpsc::Sender<Message>) {
    loop {
        let index = {
            let mut schedule = shared.lock();
            loop {
                if schedule.stop || cancel.load(Ordering::SeqCst) { return; }
                if let Some(index) = next_ready(&schedule.slots, &batch.scope.regions) {
                    schedule.slots[index] = Slot::InFlight;
                    break index;
                }
                if !schedule.slots.contains(&Slot::Queued) { return; }
                schedule = shared.changed.wait(schedule).unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let _ = tx.send(Message::Started(index));
        let (outcome, reached) = attempt(batch, renderer, shared, cancel, index);
        let stops = matches!(&outcome, Outcome::Failed(code) if stops_run(code) || batch.lost_authority());
        {
            // Stopped here, under the lock the next pick takes, and not when
            // the run's thread reads the message: this worker must not start
            // another region on authority it just found gone.
            let mut schedule = shared.lock();
            schedule.slots[index] = Slot::Done;
            schedule.attempts.remove(&index);
            schedule.stop |= stops;
        }
        shared.changed.notify_all();
        let _ = tx.send(Message::Finished { index, outcome, reached, stops });
    }
}

/// One region: re-check it against the grant, mint and spend its own grant,
/// render. Answers whether it reached the renderer.
fn attempt(batch: &Batch<'_>, renderer: &dyn RegionRenderer, shared: &Shared,
    cancel: &AtomicBool, index: usize) -> (Outcome, bool) {
    let scope = batch.scope;
    let region = &scope.regions[index];
    if let Err(code) = (batch.allowed)() {
        return (Outcome::Failed(code.to_owned()), false);
    }
    let input = match (batch.read)(batch.job_path, region.page_index, &region.id, &scope.recipe) {
        Ok(input) => input,
        Err(InferenceServiceError::Consent(
            ConsentError::RegionNotFound(_) | ConsentError::PageNotFound(..) | ConsentError::PageMismatch,
        )) => return (Outcome::Skipped("gone"), false),
        Err(error) => return (Outcome::Failed(error.code().to_owned()), false),
    };
    if input.revision_hash != region.revision_hash {
        return (Outcome::Skipped("changed"), false);
    }
    let Ok(digest) = compute_operation_digest(&OperationIntent::CloudClean) else {
        return (Outcome::Failed("consent_invalid".into()), false);
    };
    let grant_scope = input.scope(scope.provider, &scope.profile_id, &scope.endpoint_fingerprint, digest,
        &scope.recipe, &region.id);
    let grant = match batch.grants.issue_grant_with_epoch(grant_scope, scope.profile_epoch, REGION_GRANT_TTL, 1) {
        Ok(grant) => grant,
        Err(GrantError::ProfileMutated | GrantError::Revoked) =>
            return (Outcome::Failed("cloud_run_profile_changed".into()), false),
        Err(_) => return (Outcome::Failed("consent_invalid".into()), false),
    };
    {
        // Registered under the lock the halt takes, so a halt either sees this
        // attempt or is seen here.
        let mut schedule = shared.lock();
        if schedule.stop || cancel.load(Ordering::SeqCst) {
            drop(schedule);
            let _ = batch.grants.revoke_grant(&grant.nonce);
            return (Outcome::Left, false);
        }
        schedule.attempts.insert(index, attempt_id_for_nonce(&grant.nonce));
    }
    if cancel.load(Ordering::SeqCst) {
        let _ = batch.grants.revoke_grant(&grant.nonce);
        return (Outcome::Left, false);
    }
    let outcome = match renderer.render(&grant.nonce, region.page_index, &region.id, &scope.recipe) {
        Ok(api) => Outcome::Cleaned(Box::new(api)),
        Err(code) => match code.as_str() {
            "cancelled" | "remote_cancelled" => Outcome::Left,
            // The region moved on while it rendered: the attach refused it.
            "region_changed" => Outcome::Skipped("changed"),
            "region_not_found" => Outcome::Skipped("gone"),
            _ => Outcome::Failed(code),
        },
    };
    (outcome, true)
}

/// The app's render path: [`InferenceService::execute_cloud_render`] with the
/// grant's target and recipe.
struct ServiceRenderer {
    app: tauri::AppHandle,
    service: InferenceService,
    job_path: PathBuf,
    target: ExecutionTarget,
    config: InferenceConfig,
    client: CloudHttpClient,
}

impl RegionRenderer for ServiceRenderer {
    fn render(&self, nonce: &str, page_index: u32, region_id: &str, recipe: &RenderRecipe) -> Result<ApiRegion, String> {
        self.service.execute_cloud_render(nonce, &self.job_path, page_index, region_id, &self.target,
            recipe, &OperationIntent::CloudClean, &self.config, crate::inference::cloud_allowed(&self.app),
            &PollOptions::interactive())
            .map_err(|e| e.code().to_owned())
    }

    fn deployment(&self) -> Result<(RenderRecipe, Option<(String, f64)>), String> {
        // The same two reads prepare made, so a price it could not read is
        // read the same way here.
        Ok((CleanGateway::recipe(&self.client)?, CleanGateway::gpu_price(&self.client)))
    }

    fn release_idle(&self) {
        if let Err(code) = gpu::release_idle(&self.client, GpuRole::Render) {
            eprintln!("manga-cleaner: render GPU idle release skipped: {code}");
        }
    }
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

/// The selected cloud profile. Whether cloud engines are on is asked apart.
struct Selected {
    config: InferenceConfig,
    target: ExecutionTarget,
    provider: CloudProvider,
    profile_id: String,
    profile_name: String,
    endpoint_fingerprint: String,
}

/// The selected profile: where a new plan goes.
fn selected(app: &tauri::AppHandle) -> Result<Selected, String> {
    let not_active = || "cloud_clean_profile_not_active".to_string();
    let config = crate::inference::config::read_inference_config(app).map_err(|_| not_active())?;
    let target = config.selected_target.clone();
    let (Some(provider), Some(profile_id)) = (target.provider(), target.profile_id().map(str::to_owned)) else {
        return Err(not_active());
    };
    destination(config, provider, profile_id)
}

/// A configured profile, selected or not: where a granted batch goes.
fn configured(app: &tauri::AppHandle, provider: CloudProvider, profile_id: String) -> Result<Selected, String> {
    let config = crate::inference::config::read_inference_config(app)
        .map_err(|_| "cloud_clean_profile_not_active".to_string())?;
    destination(config, provider, profile_id)
}

fn destination(config: InferenceConfig, provider: CloudProvider, profile_id: String) -> Result<Selected, String> {
    let not_active = || "cloud_clean_profile_not_active".to_string();
    let target = match provider {
        CloudProvider::Beam => ExecutionTarget::Beam { profile_id: profile_id.clone() },
        CloudProvider::Modal => ExecutionTarget::Modal { profile_id: profile_id.clone() },
    };
    let profile = match provider {
        CloudProvider::Beam => config.beam_profiles.get(&profile_id),
        CloudProvider::Modal => config.modal_profiles.get(&profile_id),
    }.ok_or_else(not_active)?;
    let endpoint_fingerprint = crate::inference::config::compute_canonical_endpoint_fingerprint(&profile.endpoint_url)
        .map_err(|_| "endpoint_invalid".to_string())?;
    let profile_name = profile.name.clone();
    Ok(Selected { config, target, provider, profile_id, profile_name, endpoint_fingerprint })
}

fn client_for(selected: &Selected) -> Result<CloudHttpClient, String> {
    build_client_for_profile(&selected.config, selected.provider, &selected.profile_id).map_err(|e| match e {
        BuildClientError::CredentialMissing(_) => "credential_missing".to_string(),
        BuildClientError::Configuration(_) => "cloud_clean_profile_not_active".to_string(),
    })
}

/// The selected profile's gateway, opened on its first read. [`prepare`]
/// reads it only when something is in scope, so an empty scope never asks
/// the cloud switch, the keychain or the network.
struct LazyGateway<'a> {
    selected: &'a Selected,
    cloud_allowed: bool,
    client: OnceLock<Result<CloudHttpClient, String>>,
}

impl LazyGateway<'_> {
    fn client(&self) -> Result<&CloudHttpClient, String> {
        if !self.cloud_allowed { return Err("cloud_disabled".into()); }
        self.client.get_or_init(|| client_for(self.selected)).as_ref().map_err(Clone::clone)
    }
}

impl CleanGateway for LazyGateway<'_> {
    fn recipe(&self) -> Result<RenderRecipe, String> {
        CleanGateway::recipe(self.client()?)
    }

    fn gpu_price(&self) -> Option<(String, f64)> {
        CleanGateway::gpu_price(self.client().ok()?)
    }
}

/// Whether a running batch still speaks for its profile: cloud engines on and
/// that profile still configured, selected or not.
fn in_service(app: &tauri::AppHandle, provider: CloudProvider, profile_id: &str) -> Result<(), &'static str> {
    crate::inference::profile_in_service(app, provider, profile_id).map_err(|lost| match lost {
        crate::inference::Withdrawn::CloudDisabled => "cloud_disabled",
        crate::inference::Withdrawn::ProfileGone => "cloud_run_profile_changed",
    })
}

/// The journal's question for a chapter file: its attempts are scoped by the
/// manifest's stem, the chapter id the render path records.
fn journal_unresolved(journal: crate::inference::journal::AttemptJournal, job_path: &Path)
    -> impl Fn(u32, &str) -> bool + Sync {
    let chapter = job_path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default().to_owned();
    // An unreadable scope blocks its region rather than risk a second render.
    move |page, id| journal.unresolved_region_attempt(Some(&chapter), Some(page), id).map_or(true, |found| found.is_some())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn prepare_cloud_clean(
    app: tauri::AppHandle,
    chapter_id: String,
    scope: String,
    page_indices: Option<Vec<u32>>,
    region_ids: Option<Vec<String>>,
    local_first: Option<bool>,
    bubble_engine: Option<String>,
    outside_engine: Option<String>,
    qwen_edit: Option<cleaner_core::engines::render::QwenEdit>,
) -> Result<CloudCleanProposal, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let selected = selected(&app)?;
        // A run holds its pages' job lock page by page; its plan would be
        // stale as soon as it was read. A run on another chapter reads and
        // writes none of this one's.
        if run::chapter_run(&chapter_id).is_some() { return Err("cloud_clean_run_active".to_string()); }
        let gateway = LazyGateway {
            selected: &selected, cloud_allowed: crate::inference::cloud_allowed(&app), client: OnceLock::new(),
        };
        let library = Library::for_app(&app).map_err(|e| e.to_string())?;
        let path = library.resolve_chapter(&chapter_id).map_err(|e| e.to_string())?;
        let unresolved = journal_unresolved(app_inference_service(&app)?.journal().clone(), &path);
        let standing = library.cloud_consent_covers(&chapter_id, selected.provider,
            &selected.profile_id, &selected.endpoint_fingerprint);
        // Text cleanup sends its two picks. The region menu sends none, or
        // under the mixed choice a flat-colour bubble pick alone; a side not
        // sent keeps each region's saved pick.
        let settings = crate::settings::read(&app).unwrap_or(serde_json::Value::Null);
        let stored = settings.get("engineCeiling").and_then(|value| value.as_str());
        let picks = run::Repick::from_args(bubble_engine.as_deref(), outside_engine.as_deref(),
            run::effective_ceiling(None, stored));
        let mut proposal = prepare(&path, PrepareRequest {
            // Strictly the cloud unless mixed is asked for by name.
            chapter_id, scope, page_indices, region_ids, local_first: local_first.unwrap_or(false), picks, qwen_edit,
        }, Destination {
            provider: selected.provider,
            profile_id: selected.profile_id.clone(),
            profile_name: selected.profile_name.clone(),
            endpoint_fingerprint: selected.endpoint_fingerprint.clone(),
        }, &gateway, &unresolved)?;
        proposal.standing = standing;
        Ok(proposal)
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn confirm_cloud_clean(
    app: tauri::AppHandle, proposal_id: String, plan_digest: String, rights_attested: bool,
    retention_acknowledged: bool,
) -> Result<CloudCleanGrant, String> {
    if !crate::inference::cloud_allowed(&app) {
        return Err("cloud_disabled".into());
    }
    let library = Library::for_app(&app).ok();
    confirm_in_project(library.as_ref(), &proposal_id, &plan_digest, rights_attested, retention_acknowledged)
}

#[tauri::command]
pub async fn start_cloud_clean(app: tauri::AppHandle, grant_id: String) -> Result<RunHandle, String> {
    tauri::async_runtime::spawn_blocking(move || start(&app, &grant_id)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel_cloud_clean(proposal_id: String) -> Result<bool, String> {
    discard(&proposal_id)
}

fn start(app: &tauri::AppHandle, grant_id: &str) -> Result<RunHandle, String> {
    // Answered as `run_clean` answers it, and before the grant is spent, so
    // the consent can still start the batch once the other run is over. The
    // chapter is read from the grant without spending it.
    let chapter = grant_chapter(grant_id).ok_or("cloud_clean_grant_missing")?;
    let reservation = match run::reserve_run(&chapter, &[], run::RunKind::CloudClean)? {
        Ok(reservation) => reservation,
        Err(busy) => return Ok(busy),
    };
    // The profile the grant was given for, not the one selected now.
    let (provider, profile_id) = grant_profile(grant_id).ok_or("cloud_clean_grant_missing")?;
    let selected = configured(app, provider, profile_id)?;
    if !crate::inference::cloud_allowed(app) { return Err("cloud_disabled".into()); }
    let client = client_for(&selected)?;
    let recipe = CleanGateway::recipe(&client)?;
    let scope = consume_grant(grant_id, &CleanGrantRequest {
        provider: selected.provider,
        profile_id: &selected.profile_id,
        endpoint_fingerprint: &selected.endpoint_fingerprint,
        profile_epoch: GrantService::global().get_profile_epoch(selected.provider, &selected.profile_id),
        recipe: &recipe,
    })?;
    let job_path = Library::for_app(app).map_err(|e| e.to_string())?
        .resolve_chapter(&scope.chapter_id).map_err(|e| e.to_string())?;
    let pages: Vec<QueuedPage> = scope.regions.iter().map(|region| region.page_index)
        .collect::<BTreeSet<_>>().into_iter()
        .map(|page_index| QueuedPage {
            chapter_id: scope.chapter_id.clone(),
            page_id: library::page_id(&scope.chapter_id, page_index as usize),
            page_index,
        }).collect();
    let service = app_inference_service(app)?.with_review_retry(false);
    let unresolved = journal_unresolved(service.journal().clone(), &job_path);
    let renderer = ServiceRenderer {
        app: app.clone(),
        service,
        job_path: job_path.clone(),
        target: selected.target,
        config: selected.config,
        client,
    };
    let chapter_id = scope.chapter_id.clone();
    let app = app.clone();
    Ok(run::start_reserved_in_slot(reservation, &chapter_id, pages, move |run_id, cancel, emit| {
        let allowed = || in_service(&app, scope.provider, &scope.profile_id);
        let local_pass = |ids: &[String]| run::clean_fill_rung_first(&job_path, ids, cancel);
        let batch = Batch {
            scope: &scope, job_path: &job_path, grants: GrantService::global(), allowed: &allowed,
            unresolved: &unresolved, local_pass: Some(&local_pass),
            read: &read_render_input,
        };
        run_plan(&batch, &renderer, run_id, cancel, emit)
    }))
}

#[cfg(test)]
mod tests;
