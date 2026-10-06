//! Cloud page denoise: whole pages through a waifu2x or MangaJPEG recipe on the
//! user's own analysis GPU (`/mc/denoise/v1`, `deploy/cloud/common/denoise.py`).
//!
//! ## Three calls, and nothing sent before the third
//!
//! A denoise upload is a whole page, cleaned or not, so it takes the consent
//! every other page upload takes (`docs/cloud-security.md` §4):
//!
//! 1. [`prepare`] states the plan and does none of it: the chapter, the pages
//!    and the appearance each is planned at ([`crate::tile::page_appearance`]),
//!    the recipe and its pinned models, the endpoint, and a GPU time and cost
//!    range. It reads the gateway's denoise capabilities and GPU price and the
//!    pages' declared sizes. No page data leaves the computer.
//! 2. [`confirm`] takes the two consent answers and the plan digest the dialog
//!    showed, and mints a single-use **denoise grant** valid for
//!    [`DENOISE_GRANT_TTL`].
//! 3. `start_cloud_denoise` spends it ([`consume_grant`]) and runs the plan
//!    headless ([`run_plan`]), writing `<out_dir>/<page file stem>.png`.
//!
//! ## Each page is its own upload
//!
//! Before each page the run checks its authority again (cloud engines on, the
//! profile still selected, its epoch unchanged) and stops the plan if any has
//! moved. It reads the page as the `tile://` protocol draws its `cleaned`
//! variant ([`cleaned_page`]: the source and its visible patches through
//! `cleaner_core::composite`), and a page whose appearance is no longer the
//! one consent bound is skipped, never sent. A page that goes mints and spends
//! its own [`GrantService`] grant against the consent's epoch, is journalled
//! as an analysis-role attempt (the denoise worker is the analysis GPU), and
//! is sent once. A page that fails is reported and the run goes on; it is not
//! retried and not denoised locally instead.
//!
//! The upload is always an 8-bit grey or RGB PNG without alpha ([`upload_png`]),
//! so the answer is too, and it is checked to be exactly that at the page's
//! size ([`denoise_page`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine;
use cleaner_core::cloud_denoise_wire::{
    self as wire, DenoiseCapabilities, DenoiseInfo, DenoiseMetadata, DenoiseRecipe, DenoiseResult,
    ResolvedStep, CAPABILITY, VERSION,
};
use cleaner_core::engines::render::{CloudProvider, RenderRecipe};
use cleaner_core::image::{decode, encode, png_header, BitDepth, ColorMode, Format, Header, Raster};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::mask::Rect;
use cleaner_core::project::Job;
use serde::Serialize;

use crate::inference::analysis;
use crate::inference::cloud_clean::{CostRange, COLD_START_SECONDS, IDLE_WINDOW_SECONDS};
use crate::inference::commands::{build_client_for_profile, BuildClientError};
use crate::inference::gpu::{self, GpuRole, GpuStatusWire};
use crate::inference::http::{CloudHttpClient, HttpTransportError};
use crate::inference::journal::{
    AnalysisAttemptRecord, AnalysisJournal, AnalysisPhase, ANALYSIS_JOURNAL_SCHEMA_VERSION,
};
use crate::inference::policy::{GrantError, GrantScope, GrantService};
use crate::inference::run_analysis::stops_run;
use crate::inference::InferenceConfig;
use crate::library::Library;

/// How long a confirmed consent may wait for its run. It bounds the start only.
pub const DENOISE_GRANT_TTL: Duration = Duration::from_secs(120);
/// The most pages one plan covers.
pub const MAX_PLAN_PAGES: usize = 1024;
/// Each page's own grant lives only from its mint to its upload.
const PAGE_GRANT_TTL: Duration = Duration::from_secs(60);
const MAX_OPEN_PROPOSALS: usize = 16;
const MAX_OPEN_GRANTS: usize = 16;

/* Cost. A page costs a fixed overhead plus, per step, its model's pixels (the
 * page's, times the step's scale squared for a sharpen step) at a per-megapixel
 * rate; the high end adds a cold start and the idle window, as a cloud clean's
 * does. Priced at the deployment's GPU list price plus the analysis worker's
 * CPU and memory at Modal's published rates. Both per-page rates are **not
 * measured**, so the range is a guide, not a quote. */

/// GPU seconds a page costs before its pixels: transfer, decode, encode. Unmeasured.
pub const PAGE_OVERHEAD_SECONDS: f64 = 2.0;
/// GPU seconds per model megapixel. Unmeasured.
pub const SECONDS_PER_MODEL_MEGAPIXEL: f64 = 0.5;
/// The analysis worker's CPU (`cpu=2.0`) and memory with denoise on (24 GiB).
const CPU_CORES: f64 = 2.0;
const MEMORY_GIB: f64 = 24.0;
const CPU_USD_PER_CORE_SECOND: f64 = 0.000_013_1;
const MEMORY_USD_PER_GIB_SECOND: f64 = 0.000_002_22;

/* ------------------------------------------------------------------ */
/* Where denoise runs                                                  */
/* ------------------------------------------------------------------ */

/// The `denoiseTarget` setting. The backend acts on no setting here (a cloud
/// denoise starts only from its own three calls); it only keeps the value well
/// formed, since it decides where pages are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DenoiseTarget {
    Local,
    Cloud,
    #[default]
    Off,
}

pub const DENOISE_TARGET_KEY: &str = "denoiseTarget";

impl DenoiseTarget {
    /// Strict: `"local"`, `"cloud"` or `"off"`. Absent or null is off.
    /// Anything else is refused, because a typo must not decide where a page is sent.
    pub fn from_value(value: Option<&serde_json::Value>) -> Result<Self, String> {
        match value.filter(|value| !value.is_null()).map(|value| value.as_str()) {
            None | Some(Some("off")) => Ok(Self::Off),
            Some(Some("local")) => Ok(Self::Local),
            Some(Some("cloud")) => Ok(Self::Cloud),
            Some(_) => Err("denoiseTarget must be \"local\", \"cloud\" or \"off\"".into()),
        }
    }
}

/* ------------------------------------------------------------------ */
/* The gateway                                                         */
/* ------------------------------------------------------------------ */

/// What this module asks the deployment. Only [`DenoiseGateway::denoise`]
/// carries page data.
pub(crate) trait DenoiseGateway {
    fn capabilities(&self) -> Result<DenoiseCapabilities, String>;
    /// Send one page. `Err` is a stable code ([`rejection_code`]).
    fn denoise(&self, metadata: &DenoiseMetadata, page_png: &[u8]) -> Result<DenoiseResult, String>;
    /// [`Self::denoise`], given up once `cancel` is set while the page is out. The
    /// HTTP client submits and polls the page (`inference::gpu_jobs`), so it stops
    /// polling and cancels the page on the gateway (`cloud_denoise_cancelled`);
    /// a gateway that cannot (the default, an older synchronous gateway) waits.
    fn denoise_cancellable(&self, metadata: &DenoiseMetadata, page_png: &[u8], _cancel: &AtomicBool)
        -> Result<DenoiseResult, String> {
        self.denoise(metadata, page_png)
    }
    /// The deployed GPU and its list price per hour, when the gateway states one.
    fn gpu_price(&self) -> Option<(String, f64)> {
        None
    }
    /// Release the analysis GPU if it is idle. Best effort.
    fn release_idle(&self) {}
}

/// A deployment prices the one GPU it runs; more than one entry states no price.
fn only_price(status: GpuStatusWire) -> Option<(String, f64)> {
    let mut prices = status.list_price_usd_per_hour.into_iter();
    let only = prices.next()?;
    if prices.next().is_some() { return None; }
    Some(only).filter(|(_, price)| price.is_finite() && *price > 0.0)
}

/// A transport failure as the code a page fails with. The gateway's own
/// rejections keep their meaning; everything else is [`gpu::transport_code`].
fn rejection_code(error: &HttpTransportError) -> String {
    match error {
        HttpTransportError::UnexpectedStatus { status: 400 } => "cloud_denoise_invalid_request".into(),
        HttpTransportError::UnexpectedStatus { status: 413 } => "cloud_denoise_page_too_large".into(),
        HttpTransportError::UnexpectedStatus { status: 422 } => "cloud_denoise_unsupported_page".into(),
        HttpTransportError::UnexpectedStatus { status: 500 } => "cloud_denoise_inference_failed".into(),
        HttpTransportError::UnexpectedStatus { status: 503 } => "capability_unavailable: denoise GPU unavailable".into(),
        // A polled page the gateway answered: its typed code keeps the meaning the
        // synchronous route's status had.
        HttpTransportError::JobFailed { code } => match code.as_str() {
            "capability_unavailable" => "capability_unavailable: denoise GPU unavailable".into(),
            "invalid_request" => "cloud_denoise_invalid_request".into(),
            "page_too_large" => "cloud_denoise_page_too_large".into(),
            "unsupported_page" => "cloud_denoise_unsupported_page".into(),
            other => format!("cloud_denoise_inference_failed: {other}"),
        },
        HttpTransportError::JobCancelled => CANCELLED.into(),
        other => gpu::transport_code(other),
    }
}

/// A page cancelled while out, and the gateway confirmed it is over.
const CANCELLED: &str = "cloud_denoise_cancelled";

impl DenoiseGateway for CloudHttpClient {
    fn capabilities(&self) -> Result<DenoiseCapabilities, String> {
        self.get_denoise_capabilities().map_err(|e| match e {
            HttpTransportError::WireValidation | HttpTransportError::JsonDeserialization =>
                "capability_unavailable: gateway advertisement is invalid".into(),
            HttpTransportError::UnexpectedStatus { status: 404 | 503 } =>
                "capability_unavailable: gateway denoise is not configured".into(),
            other => gpu::transport_code(&other),
        })
    }

    fn denoise(&self, metadata: &DenoiseMetadata, page_png: &[u8]) -> Result<DenoiseResult, String> {
        self.denoise_cancellable(metadata, page_png, &AtomicBool::new(false))
    }

    fn denoise_cancellable(&self, metadata: &DenoiseMetadata, page_png: &[u8], cancel: &AtomicBool)
        -> Result<DenoiseResult, String> {
        self.denoise_page(metadata, page_png, cancel).map_err(|e| rejection_code(&e))
    }

    fn gpu_price(&self) -> Option<(String, f64)> {
        self.get_gpu_status().ok().flatten().and_then(only_price)
    }

    fn release_idle(&self) {
        if let Err(detail) = gpu::release_idle(self, GpuRole::Analysis) {
            eprintln!("manga-cleaner: denoise GPU idle release skipped: {detail}");
        }
    }
}

/// What the deployment can denoise now. Sends no page data.
pub(crate) fn capabilities<G: DenoiseGateway + ?Sized>(gateway: &G) -> Result<DenoiseCapabilities, String> {
    let advertised = gateway.capabilities()?;
    advertised.validate().map_err(|_| "capability_unavailable: gateway advertisement is invalid")?;
    Ok(advertised)
}

/// One denoised page: the gateway's PNG, checked, and what it says it ran.
#[derive(Debug, Clone)]
pub struct DenoisedPage {
    pub png: Vec<u8>,
    pub info: DenoiseInfo,
}

/// The local checks an upload passes before anything is sent: a valid
/// recipe, an 8-bit grey or RGB PNG, inside the gateway's limits.
fn check_upload(recipe: &DenoiseRecipe, page_png: &[u8]) -> Result<Header, String> {
    recipe.resolve().map_err(|e| format!("cloud_denoise_invalid_recipe: {e}"))?;
    let header = png_header(page_png).map_err(|_| "cloud_denoise_unsupported_page: not a PNG")?;
    if !matches!(header.mode, ColorMode::Gray | ColorMode::Rgb) || header.depth != BitDepth::Eight {
        return Err("cloud_denoise_unsupported_page: an upload is 8-bit grey or RGB".into());
    }
    recipe.fits(header.width, header.height).map_err(|e| format!("cloud_denoise_page_too_large: {e}"))?;
    if page_png.len() > wire::MAX_PNG_BYTES {
        return Err("cloud_denoise_page_too_large: the page PNG is past the upload limit".into());
    }
    Ok(header)
}

/// Send one page through `recipe` and check the answer: the version and the
/// digest echo, the recipe's models in order, and a PNG of the page's size
/// within the size limit. `cancel` gives the page up while it is out
/// ([`DenoiseGateway::denoise_cancellable`]).
pub(crate) fn denoise_page<G: DenoiseGateway + ?Sized>(gateway: &G, recipe: &DenoiseRecipe, page_png: &[u8],
    cancel: &AtomicBool) -> Result<DenoisedPage, String> {
    let header = check_upload(recipe, page_png)?;
    let metadata = DenoiseMetadata::new(recipe.clone(), page_png)
        .map_err(|e| format!("cloud_denoise_invalid_recipe: {e}"))?;
    let result = gateway.denoise_cancellable(&metadata, page_png, cancel)?;
    if result.page_png_b64.len() > wire::MAX_PNG_B64_BYTES {
        return Err("gateway_protocol: denoise answer is past the size limit".into());
    }
    let png = base64::engine::general_purpose::STANDARD.decode(&result.page_png_b64)
        .map_err(|_| "gateway_protocol: denoise answer is not base64")?;
    result.validate(&metadata, header.width, header.height, &png).map_err(|e| format!("gateway_protocol: {e}"))?;
    Ok(DenoisedPage { png, info: result.info })
}

/* ------------------------------------------------------------------ */
/* Proposal and grant                                                  */
/* ------------------------------------------------------------------ */

/// What a cloud denoise would send.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudDenoiseProposal {
    pub proposal_id: String,
    pub chapter_id: String,
    /// Every page the consent covers, in the order the run takes them.
    pub page_indices: Vec<u32>,
    pub pages: u32,
    /// Pages in scope left out: past the gateway's page or upscale limits for this recipe.
    pub too_large_pages: Vec<u32>,
    pub total_pixels: u64,
    pub recipe: DenoiseRecipe,
    /// The pinned model of each step, in order.
    pub model_ids: Vec<String>,
    pub estimated_gpu_seconds: CostRange,
    /// `None` when the gateway states no GPU price.
    pub estimated_cost_usd: Option<CostRange>,
    pub gpu: Option<String>,
    pub provider: CloudProvider,
    pub profile_name: String,
    pub profile_id: String,
    /// The digest the consent is given for ([`DenoisePlan::digest`]).
    pub plan_digest: String,
    pub expires_at_ms: u64,
    /// The chapter's project already consented to this endpoint, so the
    /// question is skipped. The plan digest and its grant are not.
    pub standing: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudDenoiseGrant {
    pub grant_id: String,
    pub expires_at_ms: u64,
}

/// One page a plan covers, and the state it covers it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedPage {
    pub page_index: u32,
    pub source_idx: usize,
    /// [`crate::tile::page_appearance`]: the source and every visible patch
    /// that reaches the page. The upload is refused when it has moved.
    pub appearance: String,
    pub width: u32,
    pub height: u32,
}

/// What a denoise grant is bound to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DenoisePlan {
    pub chapter_id: String,
    pub pages: Vec<PlannedPage>,
    pub recipe: DenoiseRecipe,
    pub provider: CloudProvider,
    pub profile_id: String,
    pub endpoint_fingerprint: String,
    pub profile_epoch: u64,
    pub gpu_seconds: f64,
    pub price: Option<(String, f64)>,
    pub cost: Option<CostRange>,
    pub plan_digest: String,
}

impl DenoisePlan {
    fn seconds(&self) -> CostRange {
        CostRange { low: self.gpu_seconds, high: self.gpu_seconds + COLD_START_SECONDS + IDLE_WINDOW_SECONDS }
    }

    /// The digest of everything consent covers: chapter, pages and their
    /// appearance, the canonical recipe, the destination and the estimate.
    pub(crate) fn digest(&self) -> Result<String, String> {
        let micro = |usd: f64| (usd * 1e6).round() as u64;
        let recipe = String::from_utf8(self.recipe.canonical_bytes()?).map_err(|e| e.to_string())?;
        let pages: Vec<serde_json::Value> = self.pages.iter().map(|page| serde_json::json!(
            [page.page_index, page.source_idx, page.appearance, page.width, page.height])).collect();
        let seconds = self.seconds();
        let terms = serde_json::json!({
            "v": "mc-denoise-plan-v1",
            "chapterId": self.chapter_id,
            "pages": pages,
            "recipe": recipe,
            "provider": self.provider.as_str(),
            "profileId": self.profile_id,
            "endpoint": self.endpoint_fingerprint,
            "gpuMs": [(seconds.low * 1000.0).round() as u64, (seconds.high * 1000.0).round() as u64],
            "costMicroUsd": self.cost.map(|cost| [micro(cost.low), micro(cost.high)]),
            "gpu": self.price.as_ref().map(|(gpu, _)| gpu),
            "gpuMicroUsdPerHour": self.price.as_ref().map(|(_, price)| micro(*price)),
        });
        Ok(sha256_hex(&serde_json::to_vec(&terms).map_err(|e| e.to_string())?))
    }
}

/// GPU seconds for one page of `width` by `height` through `steps`.
fn page_seconds(steps: &[ResolvedStep], width: u32, height: u32) -> f64 {
    let megapixels = f64::from(width) * f64::from(height) / 1e6;
    PAGE_OVERHEAD_SECONDS + steps.iter()
        .map(|step| megapixels * f64::from(step.scale * step.scale) * SECONDS_PER_MODEL_MEGAPIXEL)
        .sum::<f64>()
}

fn cost(gpu_seconds: f64, gpu_usd_per_hour: f64) -> CostRange {
    let per_second = gpu_usd_per_hour / 3600.0 + CPU_CORES * CPU_USD_PER_CORE_SECOND
        + MEMORY_GIB * MEMORY_USD_PER_GIB_SECOND;
    CostRange {
        low: gpu_seconds * per_second,
        high: (gpu_seconds + COLD_START_SECONDS + IDLE_WINDOW_SECONDS) * per_second,
    }
}

struct StoredProposal {
    expires_at_ms: u64,
    plan: DenoisePlan,
}

struct StoredGrant {
    expires_at_ms: u64,
    plan: DenoisePlan,
}

fn proposals() -> &'static Mutex<HashMap<String, StoredProposal>> {
    static PROPOSALS: OnceLock<Mutex<HashMap<String, StoredProposal>>> = OnceLock::new();
    PROPOSALS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn grants() -> &'static Mutex<HashMap<String, StoredGrant>> {
    static GRANTS: OnceLock<Mutex<HashMap<String, StoredGrant>>> = OnceLock::new();
    GRANTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Where the plan would go: the selected profile.
pub(crate) struct Destination {
    pub provider: CloudProvider,
    pub profile_id: String,
    pub profile_name: String,
    pub endpoint_fingerprint: String,
}

/// State what a cloud denoise would send. Nothing is read beyond the manifest,
/// nothing is written, and nothing is sent.
pub(crate) fn prepare<G: DenoiseGateway + ?Sized>(
    job_path: &Path,
    chapter_id: String,
    page_indices: Option<Vec<u32>>,
    recipe: DenoiseRecipe,
    destination: Destination,
    gateway: &G,
) -> Result<CloudDenoiseProposal, String> {
    let steps = recipe.resolve().map_err(|e| format!("cloud_denoise_invalid_recipe: {e}"))?;
    let advertised = capabilities(gateway)?;
    if !advertised.supports(&recipe) {
        return Err("capability_unavailable: the deployment has no models for this recipe".into());
    }
    let profile_epoch = GrantService::global().get_profile_epoch(destination.provider, &destination.profile_id);
    let (mut pages, mut too_large) = (Vec::new(), Vec::new());
    {
        // A read, so without the lock: a run walking this chapter holds it
        // for the whole run (see `crate::run::lock_job`).
        let job = Job::open(job_path).map_err(|e| e.to_string())?;
        let count = job.project.strip.order.len() as u32;
        let mut positions = page_indices.unwrap_or_else(|| (0..count).collect());
        positions.sort_unstable();
        positions.dedup();
        if positions.is_empty() { return Err("cloud_denoise_no_pages".into()); }
        if positions.len() > MAX_PLAN_PAGES { return Err("cloud_denoise_too_many_pages".into()); }
        for page_index in positions {
            let source_idx = Library::resolve_page(&job.project, page_index as usize)
                .ok_or("cloud_denoise_page_missing")?;
            let declared = job.project.sources.get(source_idx).ok_or("cloud_denoise_page_missing")?;
            let (width,height) = declared.orientation.size(declared.w,declared.h);
            if recipe.fits(width, height).is_err() {
                too_large.push(page_index);
                continue;
            }
            pages.push(PlannedPage {
                page_index, source_idx,
                appearance: crate::tile::page_appearance(&job.project, page_index as usize),
                width, height,
            });
        }
    }
    if pages.is_empty() { return Err("cloud_denoise_nothing_to_send".into()); }
    let gpu_seconds = pages.iter().map(|page| page_seconds(&steps, page.width, page.height)).sum();
    let price = gateway.gpu_price();
    let mut plan = DenoisePlan {
        chapter_id, pages, recipe, provider: destination.provider, profile_id: destination.profile_id,
        endpoint_fingerprint: destination.endpoint_fingerprint, profile_epoch, gpu_seconds,
        cost: price.as_ref().map(|(_, usd)| cost(gpu_seconds, *usd)), price, plan_digest: String::new(),
    };
    plan.plan_digest = plan.digest()?;
    open_proposal(plan, destination.profile_name, too_large)
}

/// Hold a plan for confirmation and state it.
pub(crate) fn open_proposal(plan: DenoisePlan, profile_name: String, too_large_pages: Vec<u32>)
    -> Result<CloudDenoiseProposal, String> {
    let now = analysis::now_ms();
    let expires_at_ms = now.saturating_add(crate::inference::consent::DEFAULT_PROPOSAL_TTL.as_millis() as u64);
    let proposal = CloudDenoiseProposal {
        proposal_id: analysis::random_id()?,
        chapter_id: plan.chapter_id.clone(),
        page_indices: plan.pages.iter().map(|page| page.page_index).collect(),
        pages: plan.pages.len() as u32,
        too_large_pages,
        total_pixels: plan.pages.iter().map(|page| u64::from(page.width) * u64::from(page.height)).sum(),
        model_ids: plan.recipe.resolve()?.into_iter().map(|step| step.model_id).collect(),
        recipe: plan.recipe.clone(),
        estimated_gpu_seconds: plan.seconds(),
        estimated_cost_usd: plan.cost,
        gpu: plan.price.as_ref().map(|(gpu, _)| gpu.clone()),
        provider: plan.provider,
        profile_name,
        profile_id: plan.profile_id.clone(),
        plan_digest: plan.plan_digest.clone(),
        expires_at_ms,
        standing: false,
    };
    let mut cache = proposals().lock().map_err(|e| e.to_string())?;
    cache.retain(|_, stored| stored.expires_at_ms > now);
    if cache.len() >= MAX_OPEN_PROPOSALS { return Err("cloud_denoise_proposal_limit".into()); }
    // A profile edited while the pages were read makes this plan stale at once.
    if GrantService::global().get_profile_epoch(plan.provider, &plan.profile_id) != plan.profile_epoch {
        return Err("cloud_denoise_profile_changed".into());
    }
    cache.insert(proposal.proposal_id.clone(), StoredProposal { expires_at_ms, plan });
    Ok(proposal)
}

/// Turn a proposal the user confirmed into a denoise grant, only for the plan
/// the dialog showed. Missing consent leaves the proposal open; any other
/// answer, a mismatched plan included, spends it.
pub(crate) fn confirm(proposal_id: &str, plan_digest: &str, rights_attested: bool, retention_acknowledged: bool)
    -> Result<CloudDenoiseGrant, String> {
    analysis::require_consent(rights_attested, retention_acknowledged).map_err(|e| e.to_string())?;
    let stored = proposals().lock().map_err(|e| e.to_string())?
        .remove(proposal_id).ok_or("cloud_denoise_proposal_missing")?;
    let now = analysis::now_ms();
    if now >= stored.expires_at_ms { return Err("cloud_denoise_proposal_expired".into()); }
    let plan = stored.plan;
    if plan_digest != plan.plan_digest { return Err("cloud_denoise_plan_mismatch".into()); }
    if GrantService::global().get_profile_epoch(plan.provider, &plan.profile_id) != plan.profile_epoch {
        return Err("cloud_denoise_profile_changed".into());
    }
    let grant = CloudDenoiseGrant {
        grant_id: analysis::random_id()?,
        expires_at_ms: now.saturating_add(DENOISE_GRANT_TTL.as_millis() as u64),
    };
    let mut store = grants().lock().map_err(|e| e.to_string())?;
    store.retain(|_, entry| entry.expires_at_ms > now);
    if store.len() >= MAX_OPEN_GRANTS { return Err("cloud_denoise_grant_limit".into()); }
    store.insert(grant.grant_id.clone(), StoredGrant { expires_at_ms: grant.expires_at_ms, plan });
    Ok(grant)
}

/// Where an open proposal would send its pages, for the project's standing consent.
fn proposal_destination(proposal_id: &str) -> Option<(String, CloudProvider, String, String)> {
    let cache = proposals().lock().ok()?;
    let plan = &cache.get(proposal_id)?.plan;
    Some((plan.chapter_id.clone(), plan.provider, plan.profile_id.clone(), plan.endpoint_fingerprint.clone()))
}

/// [`confirm`] for a proposal in `library`'s projects, as a cloud clean is
/// confirmed: a standing consent answers the statements, and only an answer
/// that ticked both is recorded to stand.
pub(crate) fn confirm_in_project(library: Option<&Library>, proposal_id: &str, plan_digest: &str,
    rights_attested: bool, retention_acknowledged: bool) -> Result<CloudDenoiseGrant, String> {
    let destination = proposal_destination(proposal_id);
    let standing = match (&destination, library) {
        (Some((chapter_id, provider, profile_id, endpoint)), Some(library)) =>
            library.cloud_consent_covers(chapter_id, *provider, profile_id, endpoint),
        _ => false,
    };
    let grant = confirm(proposal_id, plan_digest, rights_attested || standing, retention_acknowledged || standing)?;
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

/// The chapter an open grant denoises, read without spending it.
pub(crate) fn grant_chapter(grant_id: &str) -> Option<String> {
    grants().lock().ok()?.get(grant_id).map(|stored| stored.plan.chapter_id.clone())
}

/// The provider and profile an open grant denoises on, read without spending it.
fn grant_profile(grant_id: &str) -> Option<(CloudProvider, String)> {
    grants().lock().ok()?.get(grant_id).map(|stored| (stored.plan.provider, stored.plan.profile_id.clone()))
}

/// What a starting run presents: the destination as it is now.
pub(crate) struct GrantRequest<'a> {
    pub provider: CloudProvider,
    pub profile_id: &'a str,
    pub endpoint_fingerprint: &'a str,
    pub profile_epoch: u64,
}

/// Spend a denoise grant. It is gone after the first presentation, matched or
/// not, so a refused grant cannot be tried again with other arguments.
pub(crate) fn consume_grant(grant_id: &str, request: &GrantRequest<'_>) -> Result<DenoisePlan, String> {
    let stored = grants().lock().map_err(|e| e.to_string())?
        .remove(grant_id).ok_or("cloud_denoise_grant_missing")?;
    if analysis::now_ms() >= stored.expires_at_ms { return Err("cloud_denoise_grant_expired".into()); }
    let plan = stored.plan;
    if plan.provider != request.provider || plan.profile_id != request.profile_id {
        return Err("cloud_denoise_grant_mismatch: profile".into());
    }
    if plan.endpoint_fingerprint != request.endpoint_fingerprint {
        return Err("cloud_denoise_grant_mismatch: endpoint".into());
    }
    if plan.profile_epoch != request.profile_epoch { return Err("cloud_denoise_profile_changed".into()); }
    Ok(plan)
}

/* ------------------------------------------------------------------ */
/* The run                                                             */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DenoisedOutput {
    pub page_index: u32,
    pub path: String,
    pub gray: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PageFailure {
    pub page_index: u32,
    pub code: String,
}

/// What a run did, page by page. A failed page never stops the others unless
/// the run's authority went with it.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CloudDenoiseReport {
    pub written: Vec<DenoisedOutput>,
    pub failed: Vec<PageFailure>,
    /// A run stopped by `cancel_denoise_local` before its last page. Pages
    /// not reached are in neither list.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
}

/// Reads one planned page as the PNG to upload. Refuses a page whose
/// appearance is no longer the planned one.
pub(crate) type ReadPage<'a> = dyn Fn(&PlannedPage) -> Result<Vec<u8>, String> + Sync + 'a;

pub(crate) struct Batch<'a> {
    pub plan: &'a DenoisePlan,
    pub out_dir: &'a Path,
    /// Each page's output file stem, by page index.
    pub names: &'a HashMap<u32, String>,
    pub gateway: &'a (dyn DenoiseGateway + Sync),
    pub grants: &'a GrantService,
    pub journal: Option<&'a AnalysisJournal>,
    /// Whether the run still speaks for its profile: cloud on, profile configured.
    pub allowed: &'a (dyn Fn() -> Result<(), &'static str> + Sync),
    pub read: &'a ReadPage<'a>,
}

impl Batch<'_> {
    fn authority(&self) -> Result<(), String> {
        (self.allowed)().map_err(str::to_owned)?;
        if self.grants.get_profile_epoch(self.plan.provider, &self.plan.profile_id) != self.plan.profile_epoch {
            return Err("cloud_denoise_profile_changed".into());
        }
        Ok(())
    }
}

/// Whether a page's failure ends the run: its authority is gone, so every
/// later page would fail the same way.
fn stops(code: &str) -> bool {
    stops_run(code) || ["cloud_denoise_profile_changed", "capability_unavailable", "journal_unavailable"]
        .iter().any(|prefix| code.starts_with(prefix))
}

/// A failure the gateway answered, so nothing is in doubt remotely.
fn answered(code: &str) -> bool {
    ["cloud_denoise_", "capability_unavailable", "gateway_unauthorized", "gateway_error"]
        .iter().any(|prefix| code.starts_with(prefix))
}

fn grant_error(error: GrantError) -> String {
    match error {
        GrantError::ProfileMutated | GrantError::Revoked => "cloud_denoise_profile_changed".into(),
        other => other.to_string(),
    }
}

/// The per-page grant a run spends before the page goes.
fn page_scope(plan: &DenoisePlan, page: &PlannedPage, png: &[u8], request_digest: &str) -> Result<GrantScope, String> {
    let recipe_sha256 = sha256_hex(&plan.recipe.canonical_bytes()?);
    Ok(GrantScope {
        capability: CAPABILITY.into(),
        provider: plan.provider,
        profile_id: plan.profile_id.clone(),
        canonical_endpoint_fingerprint: plan.endpoint_fingerprint.clone(),
        source_hash: page.appearance.clone(),
        crop_sha256: sha256_hex(png),
        hint_sha256: recipe_sha256.clone(),
        operation_digest: sha256_hex(format!("denoise:{}:{}", plan.chapter_id, page.page_index).as_bytes()),
        crop_bounds: Rect::new(0, 0, page.width, page.height),
        mask_hash: page.appearance.clone(),
        revision: 0,
        input_sha256: request_digest.to_owned(),
        predecessors_sha256: String::new(),
        recipe: RenderRecipe::new(CAPABILITY, VERSION, CAPABILITY, recipe_sha256, false),
        region_ids: vec![format!("page_{}", page.page_index)],
    })
}

/// Pages a cloud run keeps in flight at once. The analysis GPU serves more
/// than one request at a time, and a page spends much of its round trip in
/// upload and download rather than on the GPU.
pub(crate) const PAGES_IN_FLIGHT: usize = 2;

/// Denoise every page of a spent plan, [`PAGES_IN_FLIGHT`] at a time, taken in
/// plan order, and report each in plan order.
///
/// Once `cancel` is set no new page is taken, and the pages in flight stop
/// waiting: the gateway is asked to cancel each ([`DenoiseGateway::denoise_cancellable`]),
/// which reports it failed with `cloud_denoise_cancelled` and journals it
/// cancelled. A gateway that cannot cancel (an older one, answering each page
/// synchronously) lets them finish and be written. Pages never taken are in
/// neither list, and the report is marked `cancelled`. `progress(done, total)`
/// hears each page end. The idle release comes after every page in flight has
/// come back, so it never races a page of this run.
pub(crate) fn run_plan(
    batch: &Batch<'_>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> CloudDenoiseReport {
    let pages = &batch.plan.pages;
    let total = pages.len();
    // Which page owns each output name, fixed before any page goes, so a
    // duplicate is the later page in plan order however the two finish.
    let mut owners: HashMap<&str, u32> = HashMap::new();
    for page in pages {
        if let Some(name) = batch.names.get(&page.page_index) {
            owners.entry(name.as_str()).or_insert(page.page_index);
        }
    }
    let next = AtomicUsize::new(0);
    let stopped: Mutex<Option<String>> = Mutex::new(None);
    // Each page's outcome by plan position (`None`: never taken), and how
    // many have ended.
    type Outcomes = (Vec<Option<Result<DenoisedOutput, String>>>, usize);
    let outcomes: Mutex<Outcomes> = Mutex::new((vec![None; total], 0));
    std::thread::scope(|threads| {
        for _ in 0..PAGES_IN_FLIGHT.min(total) {
            threads.spawn(|| loop {
                if cancel.load(Ordering::SeqCst) { break; }
                let at = next.fetch_add(1, Ordering::SeqCst);
                let Some(page) = pages.get(at) else { break };
                let stop = stopped.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
                let outcome = match stop {
                    Some(code) => Err(code),
                    None => denoise_one(batch, page, &owners, cancel),
                };
                if let Err(code) = &outcome {
                    let mut stopped = stopped.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if stopped.is_none() && stops(code) { *stopped = Some(code.clone()); }
                }
                // Counted and heard under one lock, so `done` only ever rises.
                let mut outcomes = outcomes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                outcomes.0[at] = Some(outcome);
                outcomes.1 += 1;
                progress(outcomes.1, total);
            });
        }
    });
    let (outcomes, _) = outcomes.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut report = CloudDenoiseReport::default();
    for (page, outcome) in pages.iter().zip(outcomes) {
        match outcome {
            Some(Ok(output)) => report.written.push(output),
            Some(Err(code)) => report.failed.push(PageFailure { page_index: page.page_index, code }),
            None => report.cancelled = true,
        }
    }
    // Pages given up while out end the run as cancelled too, even with none left.
    report.cancelled |= report.failed.iter().any(|failure| failure.code == CANCELLED);
    // Only a run that still speaks for the endpoint asks it anything more.
    if batch.authority().is_ok() && !report.written.is_empty() {
        batch.gateway.release_idle();
    }
    report
}

fn denoise_one(batch: &Batch<'_>, page: &PlannedPage, owners: &HashMap<&str, u32>, cancel: &AtomicBool)
    -> Result<DenoisedOutput, String> {
    batch.authority()?;
    let name = batch.names.get(&page.page_index).ok_or("cloud_denoise_page_missing")?;
    if owners.get(name.as_str()) != Some(&page.page_index) { return Err("cloud_denoise_duplicate_output".into()); }
    let native = (batch.read)(page)?;
    let png = upload_png(&native)?;
    let header = check_upload(&batch.plan.recipe, &png)?;
    if (header.width, header.height) != (page.width, page.height) {
        return Err("cloud_denoise_page_changed".into());
    }
    let digest = wire::request_digest(&batch.plan.recipe, &png)?;
    let scope = page_scope(batch.plan, page, &png, &digest)?;
    let grant = batch.grants.issue_grant_with_epoch(scope.clone(), batch.plan.profile_epoch, PAGE_GRANT_TTL, 1)
        .map_err(grant_error)?;
    batch.grants.validate_and_consume(&grant.nonce, &scope).map_err(grant_error)?;

    let mut record = match batch.journal {
        Some(journal) => {
            let mut record = AnalysisAttemptRecord {
                created_at_ms: analysis::now_ms(), schema_version: ANALYSIS_JOURNAL_SCHEMA_VERSION,
                proposal_id: analysis::random_id()?, provider: batch.plan.provider,
                profile_id: batch.plan.profile_id.clone(), capability: CAPABILITY.into(),
                source_sha256: sha256_hex(&png), underlay_sha256: digest.clone(),
                model_identity_sha256: Some(scope.hint_sha256.clone()),
                chapter_id: Some(batch.plan.chapter_id.clone()), page_index: Some(page.page_index),
                total_tiles: 1, completed_tiles: 0, reported_cost_usd: None,
                tile_submission_started: Some(false), cancel_requested: false, phase: AnalysisPhase::Confirmed,
            };
            journal.write(&record).map_err(|_| "journal_unavailable")?;
            record.phase = AnalysisPhase::SubmittedTile { index: 0 };
            record.tile_submission_started = Some(true);
            journal.write(&record).map_err(|_| "journal_unavailable")?;
            Some(record)
        }
        None => None,
    };
    let sent = denoise_page(batch.gateway, &batch.plan.recipe, &png, cancel);
    if let (Some(journal), Some(record)) = (batch.journal, record.as_mut()) {
        record.phase = match &sent {
            Ok(_) => {
                record.completed_tiles = 1;
                AnalysisPhase::RunPageComplete
            }
            // Cancelled while out, and the gateway confirmed the job is over. An
            // unconfirmed cancel is a transport code and stays unknown below.
            Err(code) if code == CANCELLED => {
                record.cancel_requested = true;
                AnalysisPhase::Cancelled
            }
            Err(code) if answered(code) => AnalysisPhase::Failed { code: code.split(':').next().unwrap_or(code).into() },
            // The page may have reached the GPU without an answer coming back.
            Err(_) => AnalysisPhase::UnknownRemoteState { index: 0 },
        };
        journal.write(record).map_err(|_| "journal_unavailable")?;
    }
    let denoised = sent?;
    let path = batch.out_dir.join(format!("{name}.png"));
    let retained = restore_denoised(&native, &denoised.png)?;
    write_atomic(&path, &retained).map_err(|e| format!("cloud_denoise_write_failed: {e}"))?;
    Ok(DenoisedOutput { page_index: page.page_index, path: path.display().to_string(), gray: denoised.info.gray })
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("page.png");
    let partial = path.with_file_name(format!(".{name}.part"));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path).inspect_err(|_| { let _ = std::fs::remove_file(&partial); })
}

/// Explicit sRGB model input. Native samples and alpha stay local for output restoration.
pub(crate) fn upload_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let page = decode(bytes).map_err(|e| format!("cloud_denoise_page_unreadable: {e}"))?;
    if !page.mode.allows_model_engines() { return Err("cloud_denoise_unsupported_page: native palette/CMYK denoise is unsupported".into()); }
    cleaner_core::engines::model::editable_color(&page).map_err(|e| format!("cloud_denoise_page_unreadable: {e}"))?;
    let display = cleaner_core::image::proxy::display(&page).map_err(|e| format!("cloud_denoise_page_unreadable: {e}"))?;
    let gray = display.is_neutral();
    let mut upload = Raster { width:page.width,height:page.height, mode:if gray {ColorMode::Gray} else {ColorMode::Rgb},depth:BitDepth::Eight,
        icc:None,palette:None,trns:None,srgb_intent:Some(1),color:Default::default(),data:Vec::new() };
    for y in 0..page.height { for x in 0..page.width {
        if gray { upload.data.push(display.sample(x,y,0) as u8); }
        else { for c in 0..3 { upload.data.push(display.sample(x,y,c) as u8); } }
    }}
    encode(&upload,Format::Png).map_err(|e| format!("cloud_denoise_page_unreadable: {e}"))
}

fn restore_denoised(source: &[u8], answer: &[u8]) -> Result<Vec<u8>,String> {
    let mut native = decode(source).map_err(|e|e.to_string())?;
    let result = decode(answer).map_err(|e|e.to_string())?;
    if (native.width,native.height)!=(result.width,result.height) { return Err("cloud_denoise_page_changed".into()); }
    let managed = cleaner_core::engines::model::editable_color(&native).map_err(|e|e.to_string())?;
    let display = cleaner_core::image::color::ManagedColor::for_raster(&result).map_err(|e|e.to_string())?;
    for y in 0..native.height { for x in 0..native.width {
        cleaner_core::engines::model::commit_srgb(&managed,&mut native,x,y,display.pixel(&result,x,y),1.0,0.0).map_err(|e|e.to_string())?;
    }}
    native.color = native.color.after_edit();
    encode(&native,Format::Png).map_err(|e|format!("cloud_denoise_write_failed: {e}"))
}

/// Why a page's cleaned composite was not read.
pub(crate) enum CleanedError {
    Unreadable(String),
    /// Its appearance is no longer the one asked for.
    Changed,
}

/// One page as the `tile://` protocol draws its `cleaned` variant at full
/// resolution: the source and its visible patches, as served. With
/// `expected`, refused when its appearance is no longer that one. Local and
/// cloud denoise both read their input here.
pub(crate) fn cleaned_composite(job_path: &Path, chapter_id: &str, page_index: u32, expected: Option<&str>)
    -> Result<Vec<u8>, CleanedError> {
    let _ = chapter_id;
    let composite = crate::tile::native_composite(job_path,page_index as usize,expected)
        .map_err(|e| CleanedError::Unreadable(e.to_string()))?.ok_or(CleanedError::Changed)?;
    let job = cleaner_core::project::Job::open_read_only(job_path).map_err(|e|CleanedError::Unreadable(e.to_string()))?;
    let index = Library::resolve_page(&job.project,page_index as usize).ok_or_else(||CleanedError::Unreadable("missing page".into()))?;
    let source_bytes = std::fs::read(job.source_path(index).ok_or_else(||CleanedError::Unreadable("missing source".into()))?).map_err(|e|CleanedError::Unreadable(e.to_string()))?;
    let source = job.display_page(index,&source_bytes).map_err(|e|CleanedError::Unreadable(e.to_string()))?;
    let composite = folded_grey(&composite,&source).unwrap_or(composite);
    encode(&composite,cleaner_core::image::lossless_format_for(&composite)).map_err(|e|CleanedError::Unreadable(e.to_string()))
}

/// Preserve the native family, depth, color interpretation and alpha while
/// preventing older tinted model patches from making a neutral source color.
fn folded_grey(composite: &Raster, source: &Raster) -> Option<Raster> {
    if !matches!(composite.mode, ColorMode::Rgb | ColorMode::Rgba) || !source.is_neutral() || composite.is_neutral() { return None; }
    let mut grey = composite.clone();
    for y in 0..composite.height { for x in 0..composite.width {
        if cleaner_core::image::color::alpha(composite,x,y)==0.0 { continue; }
        let luma = composite.luma16_at(x,y);
        let value = if composite.depth==BitDepth::Sixteen {luma} else {luma>>8};
        for c in 0..3 { grey.set_sample(x,y,c,value); }
        cleaner_core::image::color::avoid_transparent_key(&mut grey,x,y);
    }}
    Some(grey)
}

/// One page as the `tile://` protocol draws its `cleaned` variant at full
/// resolution, refused when its appearance is no longer the planned one.
fn cleaned_page(job_path: &Path, chapter_id: &str, page: &PlannedPage) -> Result<Vec<u8>, String> {
    let bytes = cleaned_composite(job_path, chapter_id, page.page_index, Some(&page.appearance)).map_err(|e| match e {
        CleanedError::Unreadable(detail) => format!("cloud_denoise_page_unreadable: {detail}"),
        CleanedError::Changed => "cloud_denoise_page_changed".to_string(),
    })?;
    Ok(bytes)
}

/// Each planned page's output stem: its source file's, as the exporter names pages.
/// A read, so without the lock a run on the chapter holds.
pub(crate) fn output_names(job_path: &Path, pages: &[PlannedPage]) -> Result<HashMap<u32, String>, String> {
    let job = Job::open(job_path).map_err(|e| e.to_string())?;
    Ok(pages.iter().filter_map(|page| {
        let stem = job.project.sources.get(page.source_idx)?.rel_path.file_stem()?.to_str()?;
        Some((page.page_index, stem.to_owned()))
    }).collect())
}

/// Keep each page a run wrote in the chapter's manifest, with the appearance
/// it was planned and read at, so the chapter can later take those files as
/// its pages (`page_denoise::replace_with_denoised`), and with what the
/// chapter's denoise history shows (`denoise_history`): the preset, where it
/// ran, whether the page had cleaning on it, and the source file it was made
/// from. Best effort: the files are written either way, and a manifest
/// another writer holds only costs the offer and the history.
///
/// Whether the page was cleaned is read here, as its planned appearance
/// against the appearance of its bare source now. Only a source taken in
/// place during the run would move that, and then the row is refused as
/// changed by the offer anyway.
pub(crate) fn record_written(
    job_path: &Path,
    pages: &[PlannedPage],
    report: &CloudDenoiseReport,
    preset: Option<&str>,
    target: wire::PresetTarget,
) {
    if report.written.is_empty() { return; }
    let recorded = crate::run::lock_job(job_path).map_err(String::from)
        .and_then(|_lock| record_held(job_path, pages, report, preset, target));
    recorded_or_logged(job_path, recorded);
}

/// The event a put-off [`record_written_soon`] sends once its rows are in,
/// naming the run: the chapter row it changes was read before them.
pub const RECORDED_EVENT: &str = "denoise://recorded";

/// [`record_written`] at once when the chapter is free, and otherwise on a
/// thread of its own that waits for it, so the run answers now. A run walking
/// the chapter holds it until its last page (`crate::run::lock_job`), and the
/// pages are already written; only their offer and history wait for it.
/// [`RECORDED_EVENT`] says when a put-off record is in.
pub(crate) fn record_written_soon(
    app: &tauri::AppHandle,
    run_id: &str,
    job_path: &Path,
    pages: &[PlannedPage],
    report: &CloudDenoiseReport,
    preset: Option<&str>,
    target: wire::PresetTarget,
) {
    if report.written.is_empty() { return; }
    if let Some(_lock) = cleaner_core::project::lock::try_lock(job_path) {
        recorded_or_logged(job_path, record_held(job_path, pages, report, preset, target));
        return;
    }
    let (app, run_id, job_path, pages, report, preset) =
        (app.clone(), run_id.to_owned(), job_path.to_owned(), pages.to_vec(), report.clone(), preset.map(str::to_owned));
    std::thread::spawn(move || {
        record_written(&job_path, &pages, &report, preset.as_deref(), target);
        use tauri::Emitter;
        let _ = app.emit(RECORDED_EVENT, serde_json::json!({ "runId": run_id }));
    });
}

/// [`record_written`]'s rows, written by a caller that holds the job.
fn record_held(
    job_path: &Path,
    pages: &[PlannedPage],
    report: &CloudDenoiseReport,
    preset: Option<&str>,
    target: wire::PresetTarget,
) -> Result<(), String> {
    let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs());
    let mut job = Job::open(job_path).map_err(|e| e.to_string())?;
    let rows: Vec<cleaner_core::project::DenoisedPage> = report.written.iter().filter_map(|output| {
        let page = pages.iter().find(|page| page.page_index == output.page_index)?;
        let bare = crate::tile::source_appearance(&job.project, page.page_index as usize);
        Some(cleaner_core::project::DenoisedPage {
            source_idx: page.source_idx,
            path: PathBuf::from(&output.path),
            appearance: page.appearance.clone(),
            created,
            preset: preset.map(str::to_owned),
            target: Some(target),
            from_cleaned: Some(page.appearance != bare),
            source: job.project.sources.get(page.source_idx).map(|source| source.rel_path.clone()),
            taken: false,
        })
    }).collect();
    job.record_denoised(rows).map_err(|e| e.to_string())
}

fn recorded_or_logged(job_path: &Path, recorded: Result<(), String>) {
    match recorded {
        Ok(()) => crate::library::invalidate_manifest_cache(job_path),
        Err(detail) => eprintln!("manga-cleaner: denoised pages not recorded for {}: {detail}", job_path.display()),
    }
}

/// The shipped preset a cloud recipe is, by its canonical bytes, for the
/// history to name. `None` for a recipe that is no preset.
pub(crate) fn preset_of(recipe: &DenoiseRecipe) -> Option<&'static str> {
    let bytes = recipe.canonical_bytes().ok()?;
    wire::PRESETS.iter().find(|preset| preset.recipe().canonical_bytes().ok().as_ref() == Some(&bytes)).map(|preset| preset.id)
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

struct Selected {
    config: InferenceConfig,
    provider: CloudProvider,
    profile_id: String,
    profile_name: String,
    endpoint_fingerprint: String,
}

/// The selected profile: where a new plan goes.
fn selected(app: &tauri::AppHandle) -> Result<Selected, String> {
    let not_active = || "cloud_denoise_profile_not_active".to_string();
    let config = crate::inference::config::read_inference_config(app).map_err(|_| not_active())?;
    let target = &config.selected_target;
    let (Some(provider), Some(profile_id)) = (target.provider(), target.profile_id().map(str::to_owned)) else {
        return Err(not_active());
    };
    destination(config, provider, profile_id)
}

/// A configured profile, selected or not: where a granted plan goes.
fn configured(app: &tauri::AppHandle, provider: CloudProvider, profile_id: String) -> Result<Selected, String> {
    let config = crate::inference::config::read_inference_config(app)
        .map_err(|_| "cloud_denoise_profile_not_active".to_string())?;
    destination(config, provider, profile_id)
}

fn destination(config: InferenceConfig, provider: CloudProvider, profile_id: String) -> Result<Selected, String> {
    let not_active = || "cloud_denoise_profile_not_active".to_string();
    let profile = match provider {
        CloudProvider::Beam => config.beam_profiles.get(&profile_id),
        CloudProvider::Modal => config.modal_profiles.get(&profile_id),
    }.ok_or_else(not_active)?;
    let endpoint_fingerprint = crate::inference::config::compute_canonical_endpoint_fingerprint(&profile.endpoint_url)
        .map_err(|_| "endpoint_invalid".to_string())?;
    let profile_name = profile.name.clone();
    Ok(Selected { config, provider, profile_id, profile_name, endpoint_fingerprint })
}

fn client_for(selected: &Selected) -> Result<CloudHttpClient, String> {
    build_client_for_profile(&selected.config, selected.provider, &selected.profile_id).map_err(|e| match e {
        BuildClientError::CredentialMissing(_) => "credential_missing".to_string(),
        BuildClientError::Configuration(_) => "cloud_denoise_profile_not_active".to_string(),
    })
}

/// Whether a running plan still speaks for its profile: cloud engines on and
/// that profile still configured, selected or not.
fn in_service(app: &tauri::AppHandle, provider: CloudProvider, profile_id: &str) -> Result<(), &'static str> {
    crate::inference::profile_in_service(app, provider, profile_id).map_err(|lost| match lost {
        crate::inference::Withdrawn::CloudDisabled => "cloud_disabled",
        crate::inference::Withdrawn::ProfileGone => "cloud_denoise_profile_changed",
    })
}

/// State what denoising `page_indices` (every page when absent) of a chapter
/// on the cloud GPU would send and cost. Sends no page data.
#[tauri::command]
pub async fn prepare_cloud_denoise(
    app: tauri::AppHandle,
    chapter_id: String,
    page_indices: Option<Vec<u32>>,
    recipe: DenoiseRecipe,
) -> Result<CloudDenoiseProposal, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !crate::inference::cloud_allowed(&app) { return Err("cloud_disabled".to_string()); }
        let selected = selected(&app)?;
        let client = client_for(&selected)?;
        let library = Library::for_app(&app).map_err(|e| e.to_string())?;
        let path = library.resolve_chapter(&chapter_id).map_err(|e| e.to_string())?;
        let standing = library.cloud_consent_covers(&chapter_id, selected.provider,
            &selected.profile_id, &selected.endpoint_fingerprint);
        let mut proposal = prepare(&path, chapter_id, page_indices, recipe, Destination {
            provider: selected.provider,
            profile_id: selected.profile_id.clone(),
            profile_name: selected.profile_name.clone(),
            endpoint_fingerprint: selected.endpoint_fingerprint.clone(),
        }, &client)?;
        proposal.standing = standing;
        Ok(proposal)
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn confirm_cloud_denoise(
    app: tauri::AppHandle, proposal_id: String, plan_digest: String, rights_attested: bool,
    retention_acknowledged: bool,
) -> Result<CloudDenoiseGrant, String> {
    if !crate::inference::cloud_allowed(&app) { return Err("cloud_disabled".into()); }
    let library = Library::for_app(&app).ok();
    confirm_in_project(library.as_ref(), &proposal_id, &plan_digest, rights_attested, retention_acknowledged)
}

/// Spend a denoise grant and denoise its pages into `out_dir`, headless. The
/// answer lists every page written and every page that failed.
///
/// `run_id` is the interface's name for the run, as a local denoise's is:
/// `denoise://progress` carries it after each page, `list_jobs` lists it and
/// `cancel_denoise_local` takes it. A caller that names none gets one minted.
#[tauri::command]
pub async fn start_cloud_denoise(app: tauri::AppHandle, grant_id: String, out_dir: String, run_id: Option<String>)
    -> Result<CloudDenoiseReport, String> {
    let run_id = run_id.unwrap_or_else(crate::page_denoise::mint_run_id);
    tauri::async_runtime::spawn_blocking(move || start(&app, &grant_id, &PathBuf::from(out_dir), &run_id))
        .await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel_cloud_denoise(proposal_id: String) -> Result<bool, String> {
    discard(&proposal_id)
}

/// The presets the selected deployment can run now, from its own
/// advertisement. Sends no page data and starts no GPU. An `Err` is a stable
/// code: `capability_unavailable: ...` when the deployment has no denoise.
#[tauri::command]
pub async fn cloud_denoise_presets(app: tauri::AppHandle, provider: Option<CloudProvider>, profile_id: Option<String>) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !crate::inference::cloud_allowed(&app) { return Err("cloud_disabled".to_string()); }
        let destination = match (provider, profile_id) {
            (Some(provider), Some(profile_id)) => configured(&app, provider, profile_id)?,
            (None, None) => selected(&app)?,
            _ => return Err("cloud_denoise_profile_not_active".into()),
        };
        let client = client_for(&destination)?;
        Ok(capabilities(&client)?.presets)
    }).await.map_err(|e| e.to_string())?
}

fn start(app: &tauri::AppHandle, grant_id: &str, out_dir: &Path, run_id: &str) -> Result<CloudDenoiseReport, String> {
    use crate::page_denoise::{DenoiseKind, DenoiseProgress, Registered, PROGRESS_EVENT};
    // First, so a Stop pressed while the grant is checked is already heard.
    // The chapter is read from the grant without spending it.
    let chapter = grant_chapter(grant_id).unwrap_or_default();
    let registered = Registered::new(run_id, DenoiseKind::Cloud, &chapter)?;
    // Checked before the grant is spent, so a bad folder does not cost the consent.
    if !out_dir.is_absolute() { return Err("cloud_denoise_out_dir_invalid".into()); }
    std::fs::create_dir_all(out_dir).map_err(|_| "cloud_denoise_out_dir_unwritable")?;
    if !crate::inference::cloud_allowed(app) { return Err("cloud_disabled".into()); }
    // The profile the grant was given for, not the one selected now.
    let (provider, profile_id) = grant_profile(grant_id).ok_or("cloud_denoise_grant_missing")?;
    let selected = configured(app, provider, profile_id)?;
    let client = client_for(&selected)?;
    let plan = consume_grant(grant_id, &GrantRequest {
        provider: selected.provider,
        profile_id: &selected.profile_id,
        endpoint_fingerprint: &selected.endpoint_fingerprint,
        profile_epoch: GrantService::global().get_profile_epoch(selected.provider, &selected.profile_id),
    })?;
    let job_path = Library::for_app(app).map_err(|e| e.to_string())?
        .resolve_chapter(&plan.chapter_id).map_err(|e| e.to_string())?;
    let names = output_names(&job_path, &plan.pages)?;
    let journal = analysis::journal_for(app)?;
    let allowed = || in_service(app, plan.provider, &plan.profile_id);
    let read = |page: &PlannedPage| cleaned_page(&job_path, &plan.chapter_id, page);
    let total = plan.pages.len();
    let emit = |done: usize, total: usize| {
        use tauri::Emitter;
        registered.progress(done, total);
        // No page's own progress comes back from the GPU: 0 while pages are
        // left, 1 once the last is in.
        let page = if done >= total { 1.0 } else { 0.0 };
        let _ = app.emit(PROGRESS_EVENT, DenoiseProgress { run_id: run_id.to_string(), done, total, page });
    };
    emit(0, total);
    let report = run_plan(&Batch {
        plan: &plan, out_dir, names: &names, gateway: &client, grants: GrantService::global(),
        journal: Some(&journal), allowed: &allowed, read: &read,
    }, registered.cancel(), &emit);
    record_written_soon(app, run_id, &job_path, &plan.pages, &report, preset_of(&plan.recipe), wire::PresetTarget::Cloud);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECIPE: &str = r#"{"schema":1,"steps":[
        {"op":"denoise","engine":"waifu2x-art-scan","level":1,"strength":0.5},
        {"op":"jpeg","engine":"mangajpeg-hq"}]}"#;

    fn recipe() -> DenoiseRecipe { serde_json::from_str(RECIPE).unwrap() }

    fn png(width: u32, height: u32, color: png::ColorType, depth: png::BitDepth) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(color);
            encoder.set_depth(depth);
            let samples = match color { png::ColorType::Rgb => 3, _ => 1 } * if depth == png::BitDepth::Sixteen { 2 } else { 1 };
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&vec![200; (width * height) as usize * samples]).unwrap();
        }
        bytes
    }

    fn rgb(width: u32, height: u32) -> Vec<u8> { png(width, height, png::ColorType::Rgb, png::BitDepth::Eight) }

    fn b64(bytes: &[u8]) -> String { base64::engine::general_purpose::STANDARD.encode(bytes) }

    /// What a working gateway answers: a grey PNG at the page's size.
    fn good_answer(metadata: &DenoiseMetadata, page: &[u8]) -> DenoiseResult {
        let header = png_header(page).unwrap();
        DenoiseResult {
            protocol_version: VERSION.into(),
            request_digest: metadata.request_digest.clone(),
            page_png_b64: b64(&png(header.width, header.height, png::ColorType::Grayscale, png::BitDepth::Eight)),
            info: DenoiseInfo {
                width: header.width, height: header.height, gray: true,
                steps: metadata.recipe.resolve().unwrap().into_iter()
                    .map(|step| wire::DenoiseStepTiming { model_id: step.model_id, ms: 3 }).collect(),
            },
        }
    }

    type Answer = dyn Fn(&DenoiseMetadata, &[u8]) -> Result<DenoiseResult, String> + Send + Sync;

    /// Counts what it is sent. A run sends from more than one thread, so the
    /// counts are atomics.
    struct Counter(AtomicUsize);

    impl Counter {
        fn get(&self) -> usize { self.0.load(Ordering::SeqCst) }
        fn bump(&self) { self.0.fetch_add(1, Ordering::SeqCst); }
    }

    struct FakeGateway {
        answer: Box<Answer>,
        sent: Counter,
        released: Counter,
    }

    impl FakeGateway {
        fn new(answer: impl Fn(&DenoiseMetadata, &[u8]) -> Result<DenoiseResult, String> + Send + Sync + 'static) -> Self {
            Self { answer: Box::new(answer), sent: Counter(AtomicUsize::new(0)), released: Counter(AtomicUsize::new(0)) }
        }
    }

    impl DenoiseGateway for FakeGateway {
        fn capabilities(&self) -> Result<DenoiseCapabilities, String> {
            Ok(serde_json::from_value(serde_json::json!({
                "protocol_version": VERSION, "available": true,
                "engines": {"denoise": ["waifu2x-art-scan"], "sharpen": [], "jpeg": ["mangajpeg-hq"]}, "models": [],
            })).unwrap())
        }

        fn denoise(&self, metadata: &DenoiseMetadata, page_png: &[u8]) -> Result<DenoiseResult, String> {
            self.sent.bump();
            (self.answer)(metadata, page_png)
        }

        fn release_idle(&self) { self.released.bump(); }
    }

    #[test]
    fn denoise_page_returns_a_checked_answer() {
        let gateway = FakeGateway::new(|metadata, page| Ok(good_answer(metadata, page)));
        let page = rgb(24, 16);
        let denoised = denoise_page(&gateway, &recipe(), &page, &AtomicBool::new(false)).unwrap();
        assert!(denoised.info.gray);
        assert_eq!(png_header(&denoised.png).unwrap().width, 24);
        assert!(capabilities(&gateway).unwrap().supports(&recipe()));
    }

    #[test]
    fn denoise_page_refuses_a_bad_answer() {
        let page = rgb(16, 16);
        type Tamper = Box<dyn Fn(&mut DenoiseResult) + Send + Sync>;
        let cases: Vec<(&str, Tamper)> = vec![
            ("digest echo", Box::new(|result| result.request_digest = "0".repeat(64))),
            ("version", Box::new(|result| result.protocol_version = "2.0.0".into())),
            ("not a PNG", Box::new(|result| result.page_png_b64 = b64(b"GIF89a, and then some more bytes to be long"))),
            ("not base64", Box::new(|result| result.page_png_b64 = "***".into())),
            ("oversize", Box::new(|result| result.page_png_b64 = "A".repeat(wire::MAX_PNG_B64_BYTES + 4))),
            ("wrong size", Box::new(|result| result.page_png_b64 = b64(&png(16, 8, png::ColorType::Grayscale, png::BitDepth::Eight)))),
            ("wrong models", Box::new(|result| { result.info.steps.reverse(); })),
        ];
        for (what, tamper) in cases {
            let gateway = FakeGateway::new(move |metadata, page| {
                let mut result = good_answer(metadata, page);
                tamper(&mut result);
                Ok(result)
            });
            let error = denoise_page(&gateway, &recipe(), &page, &AtomicBool::new(false)).unwrap_err();
            assert!(error.starts_with("gateway_protocol"), "{what}: {error}");
        }
    }

    #[test]
    fn denoise_page_refuses_before_sending() {
        let gateway = FakeGateway::new(|metadata, page| Ok(good_answer(metadata, page)));
        let sixteen = png(8, 8, png::ColorType::Grayscale, png::BitDepth::Sixteen);
        assert!(denoise_page(&gateway, &recipe(), &sixteen, &AtomicBool::new(false)).unwrap_err().starts_with("cloud_denoise_unsupported_page"));
        let mut bad = recipe();
        bad.steps[0].level = None;
        assert!(denoise_page(&gateway, &bad, &rgb(8, 8), &AtomicBool::new(false)).unwrap_err().starts_with("cloud_denoise_invalid_recipe"));
        assert!(denoise_page(&gateway, &recipe(), b"not a png", &AtomicBool::new(false)).unwrap_err().starts_with("cloud_denoise_unsupported_page"));
        assert_eq!(gateway.sent.get(), 0);
    }

    #[test]
    fn upload_is_eight_bit_without_alpha() {
        let sixteen = png(8, 8, png::ColorType::Grayscale, png::BitDepth::Sixteen);
        let header = png_header(&upload_png(&sixteen).unwrap()).unwrap();
        assert_eq!((header.mode, header.depth), (ColorMode::Gray, BitDepth::Eight));
        let grey = png(8, 8, png::ColorType::Grayscale, png::BitDepth::Eight);
        assert_eq!(png_header(&upload_png(&grey).unwrap()).unwrap().mode, ColorMode::Gray);
    }

    /// An RGBA page of one grey level, with an optional coloured pixel at (1, 1).
    #[test]
    fn cloud_denoise_restores_linear_native_depth_metadata_and_hidden_alpha_samples() {
        let source = Raster { width:2,height:1,mode:ColorMode::Rgba,depth:BitDepth::Sixteen,
            icc:None,palette:None,trns:None,srgb_intent:None,
            color:cleaner_core::image::ColorDescription{gamma:Some(100_000),..Default::default()},
            data:[1234u16,2345,3456,0,16384,16384,16384,32000].into_iter().flat_map(u16::to_be_bytes).collect() };
        let bytes=encode(&source,Format::Png).unwrap();
        let uploaded=decode(&upload_png(&bytes).unwrap()).unwrap();
        assert_eq!(uploaded.srgb_intent,Some(1));
        assert_eq!(uploaded.sample(1,0,0),137,"sRGB encoding of linear 0.25");
        let answer=Raster{width:2,height:1,mode:ColorMode::Rgb,depth:BitDepth::Eight,icc:None,palette:None,trns:None,
            srgb_intent:Some(1),color:Default::default(),data:vec![128;6]};
        let restored=decode(&restore_denoised(&bytes,&encode(&answer,Format::Png).unwrap()).unwrap()).unwrap();
        assert_eq!((restored.mode,restored.depth,restored.color.gamma),(source.mode,source.depth,Some(100_000)));
        assert_eq!(&restored.data[..8],&source.data[..8]);
        for channel in 0..3 { assert!(restored.sample(1,0,channel).abs_diff(14146)<=1,"independent IEC sRGB inverse"); }
        assert_eq!(restored.sample(1,0,3),32000);
    }

    fn rgba_page(level: u8, tint: Option<[u8; 3]>, alpha: u8) -> Raster {
        let mut page = Raster {
            width: 4, height: 3, mode: ColorMode::Rgba, depth: BitDepth::Eight, icc: None, palette: None,
            trns: None, srgb_intent: None, color: Default::default(), data: [level, level, level, alpha].repeat(12),
        };
        if let Some(rgb) = tint {
            for (c, value) in rgb.into_iter().enumerate() { page.set_sample(1, 1, c, u16::from(value)); }
        }
        page
    }

    #[test]
    fn a_coloured_patch_on_a_grey_source_goes_to_denoise_as_grey() {
        // A LaMa fill leaves a few levels of chroma; the source under it is grey.
        let source = rgba_page(180, None, 255);
        let patched = rgba_page(180, Some([190, 176, 184]), 255);
        assert!(source.is_neutral() && !patched.is_neutral());
        let grey = folded_grey(&patched, &source).expect("folded");
        assert_eq!(grey.mode, ColorMode::Rgba, "native alpha and sample family are retained");
        assert_eq!(grey.sample(0, 0, 0), 180, "an untouched pixel keeps its level");
        assert_eq!(grey.sample(1, 1, 0), 181, "the patch is its Rec. 601 luma");
        assert!(grey.is_neutral());

        let translucent = folded_grey(&rgba_page(180, Some([190, 176, 184]), 90), &source).unwrap();
        assert_eq!((translucent.mode, translucent.sample(2, 2, 3)), (ColorMode::Rgba, 90), "real alpha is kept");

        // A colour source keeps its colour, patches and all.
        assert!(folded_grey(&patched, &rgba_page(180, Some([200, 40, 40]), 255)).is_none());
    }

    #[test]
    fn denoise_target_is_strict_and_defaults_off() {
        use serde_json::json;
        assert_eq!(DenoiseTarget::from_value(None).unwrap(), DenoiseTarget::Off);
        assert_eq!(DenoiseTarget::from_value(Some(&serde_json::Value::Null)).unwrap(), DenoiseTarget::Off);
        assert_eq!(DenoiseTarget::from_value(Some(&json!("off"))).unwrap(), DenoiseTarget::Off);
        assert_eq!(DenoiseTarget::from_value(Some(&json!("local"))).unwrap(), DenoiseTarget::Local);
        assert_eq!(DenoiseTarget::from_value(Some(&json!("cloud"))).unwrap(), DenoiseTarget::Cloud);
        for bad in [json!("Cloud"), json!("gpu"), json!(false), json!({"target": "cloud"})] {
            assert!(DenoiseTarget::from_value(Some(&bad)).is_err(), "{bad}");
        }
    }

    fn plan(profile_id: &str, pages: &[u32]) -> DenoisePlan {
        let mut plan = DenoisePlan {
            chapter_id: "chapter-1".into(),
            pages: pages.iter().map(|&page_index| PlannedPage {
                page_index, source_idx: page_index as usize, appearance: format!("look{page_index}"),
                width: 16, height: 16,
            }).collect(),
            recipe: recipe(),
            provider: CloudProvider::Modal,
            profile_id: profile_id.into(),
            endpoint_fingerprint: "e".repeat(64),
            profile_epoch: GrantService::global().get_profile_epoch(CloudProvider::Modal, profile_id),
            gpu_seconds: 10.0,
            price: Some(("L4".into(), 0.8)),
            cost: Some(cost(10.0, 0.8)),
            plan_digest: String::new(),
        };
        plan.plan_digest = plan.digest().unwrap();
        plan
    }

    #[test]
    fn consent_is_bound_to_the_plan_and_spent_once() {
        let proposal = open_proposal(plan("denoise-consent", &[0, 1]), "Modal".into(), vec![]).unwrap();
        assert_eq!(proposal.page_indices, [0, 1]);
        assert_eq!(proposal.model_ids, ["waifu2x.swin_unet.art_scan.noise1", "omdb.1x-MangaJPEGHQ"]);
        assert!(confirm(&proposal.proposal_id, &proposal.plan_digest, false, true).is_err());
        assert!(confirm(&proposal.proposal_id, &"0".repeat(64), true, true).unwrap_err().contains("plan_mismatch"));
        assert!(confirm(&proposal.proposal_id, &proposal.plan_digest, true, true).unwrap_err().contains("proposal_missing"),
            "a mismatched plan spends the proposal");

        let proposal = open_proposal(plan("denoise-consent", &[0, 1]), "Modal".into(), vec![]).unwrap();
        let grant = confirm(&proposal.proposal_id, &proposal.plan_digest, true, true).unwrap();
        let endpoint = "e".repeat(64);
        let request = |epoch| GrantRequest {
            provider: CloudProvider::Modal, profile_id: "denoise-consent", endpoint_fingerprint: &endpoint, profile_epoch: epoch,
        };
        let spent = consume_grant(&grant.grant_id, &request(GrantService::global()
            .get_profile_epoch(CloudProvider::Modal, "denoise-consent"))).unwrap();
        assert_eq!(spent.plan_digest, proposal.plan_digest);
        assert!(consume_grant(&grant.grant_id, &request(0)).unwrap_err().contains("grant_missing"));

        let mut other = plan("denoise-consent", &[0, 1]);
        other.recipe.steps.pop();
        assert_ne!(other.digest().unwrap(), proposal.plan_digest, "the recipe is in the digest");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mc-denoise-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn oriented_denoise_proposal_matches_native_composite_geometry() {
        let dir=scratch("oriented-plan");
        let bytes=include_bytes!("../../../crates/cleaner-core/tests/fixtures/color-reference/orientation-6.jpg");
        let source_path=dir.join("page.jpg");std::fs::write(&source_path,bytes).unwrap();
        let source=cleaner_core::ingest::source_ref(&source_path,bytes).unwrap();
        let expected=source.orientation.size(source.width,source.height);
        let path=dir.join("chapter.mtclean");
        let project=cleaner_core::project::Project::new(&dir,"test",cleaner_core::project::StripMode::Single,&[source]);
        let job=Job::create(&path,project).unwrap();
        let gateway=FakeGateway::new(|metadata,page|Ok(good_answer(metadata,page)));
        let proposal=prepare(&path,"oriented".into(),None,recipe(),Destination {
            provider:CloudProvider::Modal,profile_id:"orientation-test".into(),profile_name:"test".into(),endpoint_fingerprint:"test".into(),
        },&gateway).unwrap();
        let plan=proposals().lock().unwrap().remove(&proposal.proposal_id).unwrap().plan;
        assert_eq!((plan.pages[0].width,plan.pages[0].height),expected);
        let composite=cleaned_page(job.path(),"oriented",&plan.pages[0]).unwrap();
        let header=cleaner_core::image::header(&composite).unwrap();
        assert_eq!((header.width,header.height),expected);
        assert_eq!(gateway.sent.get(),0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_run_collects_page_failures_and_goes_on() {
        let dir = scratch("run");
        let journal = AnalysisJournal::new(dir.join("journal"));
        let plan = plan("denoise-run", &[0, 1, 2, 3]);
        let names: HashMap<u32, String> = [(0, "001"), (1, "002"), (2, "003"), (3, "001")]
            .into_iter().map(|(index, name)| (index, name.to_string())).collect();
        let gateway = FakeGateway::new(|metadata, page| {
            // The second page sent is refused by the gateway.
            if png_header(page).unwrap().height == 17 { return Err("cloud_denoise_unsupported_page".into()); }
            Ok(good_answer(metadata, page))
        });
        let read = |page: &PlannedPage| match page.page_index {
            1 => Err("cloud_denoise_page_changed".to_string()),
            2 => Ok(rgb(16, 17)),
            _ => Ok(rgb(16, 16)),
        };
        let mut planned = plan.clone();
        planned.pages[2].height = 17;
        let allowed = || Ok(());
        let report = run_plan(&Batch {
            plan: &planned, out_dir: &dir, names: &names, gateway: &gateway, grants: GrantService::global(),
            journal: Some(&journal), allowed: &allowed, read: &read,
        }, &AtomicBool::new(false), &|_, _| {});
        assert_eq!(report.written.len(), 1);
        assert_eq!(report.written[0].page_index, 0);
        assert!(dir.join("001.png").is_file());
        let codes: Vec<(u32, &str)> = report.failed.iter().map(|f| (f.page_index, f.code.as_str())).collect();
        assert_eq!(codes, [(1, "cloud_denoise_page_changed"), (2, "cloud_denoise_unsupported_page"),
            (3, "cloud_denoise_duplicate_output")]);
        assert_eq!(gateway.sent.get(), 2, "a changed page and a duplicate name are never sent");
        assert_eq!(gateway.released.get(), 1);
        let records: Vec<AnalysisAttemptRecord> = std::fs::read_dir(dir.join("journal").join("analysis")).unwrap()
            .map(|entry| serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| record.capability == CAPABILITY && record.reported_cost_usd.is_none()));
        assert!(records.iter().any(|record| matches!(record.phase, AnalysisPhase::RunPageComplete)));
        assert!(records.iter().any(|record| matches!(&record.phase, AnalysisPhase::Failed { code } if code == "cloud_denoise_unsupported_page")));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two pages are in flight at once, so which page gets the one authority
    /// check that passes is a race; what is not is that one page goes, the
    /// other two fail with the code that stopped the run, and nothing more is
    /// sent.
    #[test]
    fn a_run_stops_sending_when_cloud_is_switched_off() {
        let dir = scratch("stop");
        let plan = plan("denoise-stop", &[0, 1, 2]);
        let names: HashMap<u32, String> = (0..3).map(|index| (index, format!("p{index}"))).collect();
        let gateway = FakeGateway::new(|metadata, page| Ok(good_answer(metadata, page)));
        let calls = AtomicUsize::new(0);
        let allowed = || {
            if calls.fetch_add(1, Ordering::SeqCst) >= 1 { Err("cloud_disabled") } else { Ok(()) }
        };
        let read = |_: &PlannedPage| Ok(rgb(16, 16));
        let report = run_plan(&Batch {
            plan: &plan, out_dir: &dir, names: &names, gateway: &gateway, grants: GrantService::global(),
            journal: None, allowed: &allowed, read: &read,
        }, &AtomicBool::new(false), &|_, _| {});
        assert_eq!(report.written.len(), 1);
        assert_eq!(report.failed.len(), 2);
        assert!(report.failed.iter().all(|failure| failure.code == "cloud_disabled"), "{:?}", report.failed);
        let mut reported: Vec<u32> = report.written.iter().map(|output| output.page_index)
            .chain(report.failed.iter().map(|failure| failure.page_index)).collect();
        reported.sort_unstable();
        assert_eq!(reported, [0, 1, 2]);
        assert!(!report.cancelled);
        assert_eq!(gateway.sent.get(), 1);
        assert_eq!(gateway.released.get(), 0, "a run without authority asks the endpoint nothing");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A gateway that holds each page until the test lets it go, and says how
    /// many pages it holds at once.
    struct GatedGateway {
        inner: FakeGateway,
        held: Mutex<usize>,
        most: AtomicUsize,
        open: (Mutex<bool>, std::sync::Condvar),
    }

    impl GatedGateway {
        fn new() -> Self {
            Self {
                inner: FakeGateway::new(|metadata, page| Ok(good_answer(metadata, page))),
                held: Mutex::new(0), most: AtomicUsize::new(0),
                open: (Mutex::new(false), std::sync::Condvar::new()),
            }
        }

        fn release_all(&self) {
            *self.open.0.lock().unwrap() = true;
            self.open.1.notify_all();
        }

        /// Wait until `count` pages are held at once, for up to five seconds.
        fn wait_held(&self, count: usize) -> bool {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if *self.held.lock().unwrap() >= count { return true; }
                std::thread::yield_now();
            }
            false
        }
    }

    impl DenoiseGateway for GatedGateway {
        fn capabilities(&self) -> Result<DenoiseCapabilities, String> { self.inner.capabilities() }

        fn denoise(&self, metadata: &DenoiseMetadata, page_png: &[u8]) -> Result<DenoiseResult, String> {
            {
                let mut held = self.held.lock().unwrap();
                *held += 1;
                self.most.fetch_max(*held, Ordering::SeqCst);
            }
            let (open, changed) = &self.open;
            let mut released = open.lock().unwrap();
            while !*released { released = changed.wait(released).unwrap(); }
            drop(released);
            *self.held.lock().unwrap() -= 1;
            self.inner.denoise(metadata, page_png)
        }

        fn release_idle(&self) { self.inner.release_idle(); }
    }

    /// Two pages go at once, never more; a cancel while they are out sends no
    /// new page, the two in flight still finish and are written (a gateway that
    /// cannot give a page up, as an older synchronous one), and the report says
    /// it was cancelled. The idle release waits for both.
    #[test]
    fn a_cancelled_run_finishes_the_pages_in_flight_and_sends_no_more() {
        let dir = scratch("cancel");
        let plan = plan("denoise-cancel", &[0, 1, 2, 3, 4]);
        let names: HashMap<u32, String> = (0..5).map(|index| (index, format!("p{index}"))).collect();
        let gateway = GatedGateway::new();
        let cancel = AtomicBool::new(false);
        let heard = Mutex::new(Vec::new());
        let allowed = || Ok(());
        let read = |_: &PlannedPage| Ok(rgb(16, 16));
        let report = std::thread::scope(|scope| {
            let run = scope.spawn(|| run_plan(&Batch {
                plan: &plan, out_dir: &dir, names: &names, gateway: &gateway, grants: GrantService::global(),
                journal: None, allowed: &allowed, read: &read,
            }, &cancel, &|done, total| heard.lock().unwrap().push((done, total))));
            assert!(gateway.wait_held(2), "two pages were not in flight at once");
            cancel.store(true, Ordering::SeqCst);
            gateway.release_all();
            run.join().unwrap()
        });
        assert_eq!(gateway.most.load(Ordering::SeqCst), 2, "more than two pages were in flight");
        assert_eq!(gateway.inner.sent.get(), 2, "a page was sent after the cancel");
        assert!(report.cancelled);
        assert_eq!(report.written.iter().map(|output| output.page_index).collect::<Vec<_>>(), [0, 1]);
        assert!(report.failed.is_empty());
        assert_eq!(*heard.lock().unwrap(), [(1, 5), (2, 5)]);
        assert_eq!(gateway.inner.released.get(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A gateway that gives a page up once the run is cancelled, as the HTTP
    /// client does on a gateway with job routes: it stops polling, the gateway
    /// confirms the cancel, and the page fails with `cloud_denoise_cancelled`.
    struct CancellingGateway {
        inner: FakeGateway,
        held: AtomicUsize,
    }

    impl DenoiseGateway for CancellingGateway {
        fn capabilities(&self) -> Result<DenoiseCapabilities, String> { self.inner.capabilities() }

        fn denoise(&self, metadata: &DenoiseMetadata, page_png: &[u8]) -> Result<DenoiseResult, String> {
            self.inner.denoise(metadata, page_png)
        }

        fn denoise_cancellable(&self, _: &DenoiseMetadata, _: &[u8], cancel: &AtomicBool) -> Result<DenoiseResult, String> {
            self.inner.sent.bump();
            self.held.fetch_add(1, Ordering::SeqCst);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !cancel.load(Ordering::SeqCst) {
                assert!(std::time::Instant::now() < deadline, "the run's cancel never reached the page");
                std::thread::yield_now();
            }
            Err(rejection_code(&HttpTransportError::JobCancelled))
        }

        fn release_idle(&self) { self.inner.release_idle(); }
    }

    /// A cancel reaches the pages in flight: they stop waiting, are reported
    /// cancelled and journalled cancelled (not unknown), nothing more is sent,
    /// and no idle release is sent for a run that wrote nothing.
    #[test]
    fn a_cancel_gives_up_the_pages_in_flight_on_a_polling_gateway() {
        let dir = scratch("cancel-polled");
        let journal = AnalysisJournal::new(dir.join("journal"));
        let plan = plan("denoise-cancel-polled", &[0, 1, 2]);
        let names: HashMap<u32, String> = (0..3).map(|index| (index, format!("p{index}"))).collect();
        let gateway = CancellingGateway {
            inner: FakeGateway::new(|metadata, page| Ok(good_answer(metadata, page))), held: AtomicUsize::new(0),
        };
        let cancel = AtomicBool::new(false);
        let allowed = || Ok(());
        let read = |_: &PlannedPage| Ok(rgb(16, 16));
        let report = std::thread::scope(|scope| {
            let run = scope.spawn(|| run_plan(&Batch {
                plan: &plan, out_dir: &dir, names: &names, gateway: &gateway, grants: GrantService::global(),
                journal: Some(&journal), allowed: &allowed, read: &read,
            }, &cancel, &|_, _| {}));
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while gateway.held.load(Ordering::SeqCst) < PAGES_IN_FLIGHT {
                assert!(std::time::Instant::now() < deadline, "two pages were not in flight");
                std::thread::yield_now();
            }
            cancel.store(true, Ordering::SeqCst);
            run.join().unwrap()
        });
        assert!(report.cancelled);
        assert!(report.written.is_empty());
        assert_eq!(report.failed.iter().map(|failure| (failure.page_index, failure.code.as_str())).collect::<Vec<_>>(),
            [(0, "cloud_denoise_cancelled"), (1, "cloud_denoise_cancelled")]);
        assert_eq!(gateway.inner.sent.get(), 2, "a page was sent after the cancel");
        assert_eq!(gateway.inner.released.get(), 0);
        let records: Vec<AnalysisAttemptRecord> = std::fs::read_dir(dir.join("journal").join("analysis")).unwrap()
            .map(|entry| serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()).collect();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| matches!(record.phase, AnalysisPhase::Cancelled) && record.cancel_requested));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A polled page's outcome keeps the meaning the synchronous status had, and
    /// only a confirmed cancel is journalled as cancelled; the rest stay unknown.
    #[test]
    fn polled_outcomes_map_to_the_synchronous_codes() {
        let failed = |code: &str| rejection_code(&HttpTransportError::JobFailed { code: code.into() });
        assert_eq!(failed("capability_unavailable"), "capability_unavailable: denoise GPU unavailable");
        assert_eq!(failed("unsupported_page"), "cloud_denoise_unsupported_page");
        assert_eq!(failed("invalid_request"), "cloud_denoise_invalid_request");
        assert_eq!(failed("worker_timeout"), "cloud_denoise_inference_failed: worker_timeout");
        assert!(answered(&failed("result_expired")));
        assert_eq!(rejection_code(&HttpTransportError::JobCancelled), CANCELLED);
        for unknown in [HttpTransportError::JobCancelUnconfirmed, HttpTransportError::JobDeadline,
            HttpTransportError::RedirectForbidden, HttpTransportError::Timeout] {
            let code = rejection_code(&unknown);
            assert_eq!(code, "gateway_unreachable");
            assert!(!answered(&code), "{unknown:?} must stay unknown in the journal");
        }
    }

    /// A run with nothing cancelled reports every page in plan order and
    /// hears each one end.
    #[test]
    fn a_run_reports_in_plan_order_with_two_in_flight() {
        let dir = scratch("order");
        let plan = plan("denoise-order", &[0, 1, 2, 3, 4]);
        let names: HashMap<u32, String> = (0..5).map(|index| (index, format!("p{index}"))).collect();
        let gateway = FakeGateway::new(|metadata, page| Ok(good_answer(metadata, page)));
        let heard = Mutex::new(Vec::new());
        let allowed = || Ok(());
        let read = |_: &PlannedPage| Ok(rgb(16, 16));
        let report = run_plan(&Batch {
            plan: &plan, out_dir: &dir, names: &names, gateway: &gateway, grants: GrantService::global(),
            journal: None, allowed: &allowed, read: &read,
        }, &AtomicBool::new(false), &|done, _| heard.lock().unwrap().push(done));
        assert_eq!(report.written.iter().map(|output| output.page_index).collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
        assert!(!report.cancelled);
        assert_eq!(*heard.lock().unwrap(), [1, 2, 3, 4, 5]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
