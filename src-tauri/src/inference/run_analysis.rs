//! Auto clean's cloud detection: RT-DETR v2 full boxes and the SAM-TS-L mask
//! from the user's own Modal or Beam endpoint, for exactly the pages a person
//! consented to.
//!
//! ## One consent per run, and nothing without it
//!
//! `runClean` has no confirmation of its own, which is why the run module keeps
//! cloud *cleaning* out of automatic runs. Detection can go to the cloud only
//! because this module is the confirmation it lacks:
//!
//! 1. [`propose_run`] states what a run would send: which chapter, which pages,
//!    which models (pinned by the gateway's advertised identity), how many
//!    tiles and pixels, to which endpoint. It reads only the gateway's model
//!    list, the pages' declared sizes and their source hashes. No page data leaves the computer.
//! 2. [`confirm_run`] takes the two consent answers and mints a **run grant**:
//!    single use, valid for [`RUN_GRANT_TTL`], and bound to the chapter, the
//!    page set, the capability set, the provider and profile, the endpoint
//!    fingerprint and the profile's mutation epoch.
//! 3. `run_clean` presents the grant once ([`consume_grant`]). Any mismatch,
//!    reuse or expiry refuses the run with a typed error before a model loads.
//!    A run whose settings route a stage to the cloud and carries no grant is
//!    refused the same way. There is no silent upload and no silent fallback.
//!
//! The run grant only has to *start* the run, so its life is short. Once spent
//! it becomes the run's [`RemoteDetection`], which the run owns and drops when
//! it ends or is cancelled: no other run can present it, and it has no clock
//! of its own, so page 80 of a chapter is not refused for taking longer than
//! the grant's two minutes. What it keeps is the consent's bounds: the
//! chapter, the exact page set, the models, and the endpoint and profile epoch.
//!
//! During the run every page mints and spends its own [`GrantService`] grant
//! against the epoch captured at consent, so editing or deleting the profile
//! mid-run stops the next upload. A page outside the granted set is refused. A
//! page whose cloud analysis fails fails on its own, with a notice, and the run
//! goes on to the next page: the failed page is neither analyzed locally
//! instead nor retried. A failure that withdraws the run's authority
//! ([`stops_run`]) ends the run instead.
//!
//! Cloud Detect uploads the current page: the source with its visible patches
//! and edits composited in display order. The raw source remains the grant identity.
//!
//! ## Detection runs ahead of the cleaning
//!
//! The analysis GPU bills by the second and scales down only after its idle
//! window, so a run that analyzed each page just before cleaning it kept the GPU
//! up, and mostly idle, for as long as this computer took to clean the chapter.
//! A run with cloud stages therefore hands its [`RemoteDetection`] to a
//! [`Prefetch`]: one producer thread that walks the run's pages in the run's own
//! order, reads each source as the run does, and analyzes it, while the run
//! takes each page's answer when it reaches that page. The GPU's work is done in
//! one burst at the start of the run, and when the producer has sent its last
//! page it asks the gateway to release the GPU if it is idle
//! ([`RemoteDetection::release_idle`]), so the idle window is not billed either.
//!
//! Nothing about consent moves. Every page still mints and spends its own grant
//! at upload time, against the consent's epoch, and is refused outside the
//! granted set; the run's cancel flag stops the producer; a failure that stops
//! the run stops the producer at that page. What changes is only *when* a page's
//! failure is seen: the producer records it, and the run reports it, with the
//! same notice, when it reaches that page, so notices stay in page order.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use cleaner_core::balloon::BalloonBox;
use cleaner_core::cloud_analysis_wire::{TileRect, RT, SAM, VERSION};
use cleaner_core::cloud_tiles;
use cleaner_core::engines::render::{CloudProvider, RenderRecipe};
use cleaner_core::image::{decode, Raster};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::mask::Rect;
use cleaner_core::project::Job;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::inference::analysis::{self, AnalysisGateway, RemoteModel, Stitched, TileUpload};
use crate::inference::journal::{AnalysisAttemptRecord, AnalysisJournal, AnalysisPhase, ANALYSIS_JOURNAL_SCHEMA_VERSION};
use crate::inference::policy::{GrantError, GrantScope, GrantService};
use crate::library::Library;

/// How long a confirmed consent may wait for the run it was given for. It
/// bounds the start only: see the module docs.
pub const RUN_GRANT_TTL: Duration = Duration::from_secs(120);
/// A consent covers one chapter; this bounds how many of its pages. The same
/// bound one review puts on its tiles.
pub const MAX_RUN_PAGES: usize = cloud_tiles::MAX_TILES;
/// And how many pixels, all pages together: sixteen reviews' worth, about
/// 280 pages of a typical 1600 by 2400 scan.
pub const MAX_RUN_TILE_PIXELS: u64 = cloud_tiles::MAX_TOTAL_PIXELS * 16;
const MAX_OPEN_PROPOSALS: usize = 16;
/// Declared page size and file size limits, the same as the review's.
const MAX_PAGE_PIXELS: u64 = 24_000_000;
const MAX_SOURCE_BYTES: u64 = 20_000_000;

/* ------------------------------------------------------------------ */
/* Where each stage runs                                               */
/* ------------------------------------------------------------------ */

/// The `analysisTargets` setting: whether the cloud-capable detection stages
/// run on the user's cloud GPU. Only the Ogkalu detector's Full profile and the
/// SAM-TS-L lettering mask have a cloud twin; CTD, the Small profile and OCR
/// are local only.
///
/// **Both stages are in one place.** The editor's *Detect on* sets them
/// together, so a split value is refused wherever it is written or passed to a
/// run ([`AnalysisTargets::from_value`]). One stored by an older build settles
/// on this computer ([`AnalysisTargets::from_stored`]): a model the user kept
/// here is never sent by a migration, and never runs here beside a cloud stage
/// without the run saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnalysisTargets {
    pub rt_full: bool,
    pub sam: bool,
}

pub const ANALYSIS_TARGETS_KEY: &str = "analysisTargets";

impl AnalysisTargets {
    /// Strict: an object whose only keys are `rtFull` and `samTs`, each
    /// `"local"` or `"cloud"`, and both the same. Absent is all local. Anything
    /// else is refused, because a typo must not decide where a page is sent.
    pub fn from_value(value: Option<&Value>) -> Result<Self, String> {
        let targets = Self::parse(value)?;
        if targets.rt_full != targets.sam {
            return Err("analysisTargets must put rtFull and samTs in the same place: detection runs either here or on the cloud GPU".into());
        }
        Ok(targets)
    }

    /// A value read back from the settings file or a job snapshot, which an
    /// older build may have written split. Malformed is still refused; split
    /// settles on this computer, the same rule the interface applies
    /// (`pipelines.js#unifyAnalysisTargets`).
    pub fn from_stored(value: Option<&Value>) -> Result<Self, String> {
        let targets = Self::parse(value)?;
        Ok(if targets.rt_full == targets.sam { targets } else { Self::default() })
    }

    fn parse(value: Option<&Value>) -> Result<Self, String> {
        let Some(value) = value.filter(|value| !value.is_null()) else { return Ok(Self::default()) };
        let map = value.as_object().ok_or("analysisTargets must be an object")?;
        let mut targets = Self::default();
        for (key, target) in map {
            let cloud = match target.as_str() {
                Some("local") => false,
                Some("cloud") => true,
                _ => return Err(format!("analysisTargets.{key} must be \"local\" or \"cloud\"")),
            };
            match key.as_str() {
                "rtFull" => targets.rt_full = cloud,
                "samTs" => targets.sam = cloud,
                _ => return Err(format!("analysisTargets has no stage {key}")),
            }
        }
        Ok(targets)
    }

    pub fn to_value(self) -> Value {
        serde_json::json!({
            "rtFull": if self.rt_full { "cloud" } else { "local" },
            "samTs": if self.sam { "cloud" } else { "local" },
        })
    }

    /// The capabilities a run with these models selected sends to the cloud,
    /// sorted. A stage routed to the cloud but not selected sends nothing.
    pub fn capabilities(self, rt_full_selected: bool, sam_selected: bool) -> Vec<String> {
        let mut capabilities = Vec::new();
        if self.sam && sam_selected { capabilities.push(SAM.to_string()); }
        if self.rt_full && rt_full_selected { capabilities.push(RT.to_string()); }
        capabilities.sort();
        capabilities
    }
}

/* ------------------------------------------------------------------ */
/* Proposal and grant                                                  */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAnalysisProposal {
    pub proposal_id: String,
    pub chapter_id: String,
    pub page_indices: Vec<u32>,
    /// Exact page sources consent covers, including the chapter's placement.
    pub page_sources: Vec<GrantedPage>,
    pub provider: CloudProvider,
    pub profile_id: String,
    pub profile_name: String,
    pub capabilities: Vec<String>,
    pub models: Vec<RemoteModel>,
    pub pages: u32,
    pub total_tiles: u64,
    pub total_tile_pixels: u64,
    /// The pages' files on disk. Tiles are re-encoded as PNG, so the upload is
    /// of the same order, not this exact figure.
    pub source_bytes: u64,
    pub includes_surrounding_art: bool,
    pub cost_estimate_usd: Option<f64>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    /// The chapter's project already consented to this endpoint, so the
    /// question is skipped. The proposal and its grant are not.
    #[serde(default)]
    pub standing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantedPage {
    pub page_index: u32,
    pub source_idx: usize,
    pub source_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunGrant {
    pub grant_id: String,
    pub chapter_id: String,
    pub page_indices: Vec<u32>,
    pub capabilities: Vec<String>,
    pub expires_at_ms: u64,
}

/// What a run grant is bound to. Every field must match when it is spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunGrantScope {
    pub chapter_id: String,
    pub page_indices: Vec<u32>,
    #[cfg(not(test))]
    pub page_sources: Vec<GrantedPage>,
    pub capabilities: Vec<String>,
    pub provider: CloudProvider,
    pub profile_id: String,
    pub endpoint_fingerprint: String,
    pub profile_epoch: u64,
    pub models: Vec<RemoteModel>,
}

/// What a starting run asks the grant to cover.
pub struct RunGrantRequest<'a> {
    pub chapter_id: &'a str,
    pub pages: &'a [u32],
    pub capabilities: &'a [String],
    pub provider: CloudProvider,
    pub profile_id: &'a str,
    pub endpoint_fingerprint: &'a str,
    pub profile_epoch: u64,
}

struct StoredProposal {
    proposal: RunAnalysisProposal,
    endpoint_fingerprint: String,
    profile_epoch: u64,
}

struct StoredGrant {
    scope: RunGrantScope,
    expires_at_ms: u64,
    #[cfg(test)]
    page_sources: Vec<GrantedPage>,
}

#[cfg(test)]
thread_local! {
    // Some older run tests construct scopes directly. A consumed test grant
    // passes its page binding to the RemoteDetection created on that thread;
    // directly constructed fake scopes stay unbound for those fixture tests.
    static TEST_CONSUMED_PAGES: std::cell::RefCell<Option<(RunGrantScope, Vec<GrantedPage>)>>
        = const { std::cell::RefCell::new(None) };
}

fn proposals() -> &'static Mutex<HashMap<String, StoredProposal>> {
    static PROPOSALS: OnceLock<Mutex<HashMap<String, StoredProposal>>> = OnceLock::new();
    PROPOSALS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn grants() -> &'static Mutex<HashMap<String, StoredGrant>> {
    static GRANTS: OnceLock<Mutex<HashMap<String, StoredGrant>>> = OnceLock::new();
    GRANTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hex(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

fn normalized_capabilities(capabilities: Vec<String>) -> Result<Vec<String>, String> {
    let mut capabilities = capabilities;
    capabilities.sort();
    capabilities.dedup();
    if capabilities.is_empty() || capabilities.iter().any(|id| !matches!(id.as_str(), SAM | RT)) {
        return Err("capability_unavailable: unsupported analysis capability".into());
    }
    Ok(capabilities)
}

fn normalized_pages(pages: Vec<u32>) -> Result<Vec<u32>, String> {
    let mut pages = pages;
    pages.sort_unstable();
    pages.dedup();
    if pages.is_empty() { return Err("cloud_run_no_pages".into()); }
    if pages.len() > MAX_RUN_PAGES { return Err("cloud_run_too_many_pages".into()); }
    Ok(pages)
}

/// State what a run would send. Reads the gateway's model list and the pages'
/// declared geometry; sends nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn propose_run<G: AnalysisGateway + ?Sized>(
    job_path: &Path,
    chapter_id: String,
    page_indices: Vec<u32>,
    capabilities: Vec<String>,
    provider: CloudProvider,
    profile_id: String,
    profile_name: String,
    endpoint_fingerprint: String,
    client: &G,
) -> Result<RunAnalysisProposal, String> {
    let capabilities = normalized_capabilities(capabilities)?;
    let page_indices = normalized_pages(page_indices)?;
    let available = client.capabilities()?;
    available.validate().map_err(|_| "capability_unavailable: gateway advertisement is invalid")?;
    let mut models = Vec::with_capacity(capabilities.len());
    for id in &capabilities {
        let advertised = available.capabilities.iter().find(|entry| &entry.capability == id)
            .ok_or("capability_unavailable: gateway does not advertise this analysis model")?;
        models.push(RemoteModel {
            capability: advertised.capability.clone(),
            graph_sha256s: advertised.graph_sha256s.clone(),
            model_revision: advertised.model_revision.clone(),
        });
    }
    let (mut total_tiles, mut total_tile_pixels, mut source_bytes) = (0u64, 0u64, 0u64);
    let mut page_sources = Vec::with_capacity(page_indices.len());
    {
        let _lock = crate::run::lock_job(job_path)?;
        let job = Job::open(job_path).map_err(|e| e.to_string())?;
        for &page in &page_indices {
            let source_idx = Library::resolve_page(&job.project, page as usize)
                .ok_or("Page is no longer in chapter")?;
            let declared = job.project.sources.get(source_idx).ok_or("Chapter source is missing")?;
            if u64::from(declared.w) * u64::from(declared.h) > MAX_PAGE_PIXELS {
                return Err("Analysis page exceeds the 24 megapixel review limit".into());
            }
            let path = job.source_path(source_idx).ok_or("Chapter source is missing")?;
            let bytes = std::fs::read(path).map_err(|_| "cloud_run_source_unreadable")?;
            if bytes.len() as u64 > MAX_SOURCE_BYTES { return Err("Source page exceeds the review-preview limit".into()); }
            page_sources.push(GrantedPage { page_index: page, source_idx, source_sha256: hex(&bytes) });
            let (width, height) = declared.orientation.size(declared.w, declared.h);
            let extent = cloud_tiles::plan(width, height, &[])?;
            if extent.tiles.iter().any(|rect| analysis::exceeds_limits(&available, *rect, None)) {
                return Err(analysis::gateway_limit_error());
            }
            total_tiles += extent.tiles.len() as u64;
            total_tile_pixels += extent.total_pixels;
            source_bytes += bytes.len() as u64;
            if total_tile_pixels > MAX_RUN_TILE_PIXELS { return Err("cloud_run_too_large".into()); }
        }
    }
    let issued_at_ms = analysis::now_ms();
    let proposal = RunAnalysisProposal {
        proposal_id: analysis::random_id()?, chapter_id, pages: page_indices.len() as u32, page_indices,
        page_sources,
        provider, profile_id, profile_name, capabilities, models,
        total_tiles, total_tile_pixels, source_bytes,
        includes_surrounding_art: true, cost_estimate_usd: None, issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(
            crate::inference::consent::DEFAULT_PROPOSAL_TTL.as_millis() as u64),
        standing: false,
    };
    let profile_epoch = GrantService::global().get_profile_epoch(provider, &proposal.profile_id);
    let mut cache = proposals().lock().map_err(|e| e.to_string())?;
    cache.retain(|_, stored| stored.proposal.expires_at_ms > issued_at_ms);
    if cache.len() >= MAX_OPEN_PROPOSALS { return Err("analysis_proposal_limit".into()); }
    cache.insert(proposal.proposal_id.clone(), StoredProposal {
        proposal: proposal.clone(), endpoint_fingerprint, profile_epoch,
    });
    Ok(proposal)
}

/// Where an open proposal would send its pages: chapter, provider, profile
/// and endpoint fingerprint, for the project's standing consent.
pub(crate) fn proposal_destination(proposal_id: &str) -> Option<(String, CloudProvider, String, String)> {
    let cache = proposals().lock().ok()?;
    let stored = cache.get(proposal_id)?;
    Some((stored.proposal.chapter_id.clone(), stored.proposal.provider,
        stored.proposal.profile_id.clone(), stored.endpoint_fingerprint.clone()))
}

/// Turn a proposal the user confirmed into a run grant. Missing consent leaves
/// the proposal open; any other answer spends it.
pub(crate) fn confirm_run(proposal_id: &str, rights_attested: bool, retention_acknowledged: bool)
    -> Result<RunGrant, String> {
    analysis::require_consent(rights_attested, retention_acknowledged).map_err(|e| e.to_string())?;
    let stored = proposals().lock().map_err(|e| e.to_string())?
        .remove(proposal_id).ok_or("analysis_proposal_missing")?;
    let now = analysis::now_ms();
    if now >= stored.proposal.expires_at_ms { return Err("analysis_proposal_expired".into()); }
    let proposal = stored.proposal;
    if GrantService::global().get_profile_epoch(proposal.provider, &proposal.profile_id) != stored.profile_epoch {
        return Err("cloud_run_profile_changed".into());
    }
    let grant = RunGrant {
        grant_id: analysis::random_id()?,
        chapter_id: proposal.chapter_id.clone(),
        page_indices: proposal.page_indices.clone(),
        capabilities: proposal.capabilities.clone(),
        expires_at_ms: now.saturating_add(RUN_GRANT_TTL.as_millis() as u64),
    };
    let mut store = grants().lock().map_err(|e| e.to_string())?;
    store.retain(|_, entry| entry.expires_at_ms > now);
    store.insert(grant.grant_id.clone(), StoredGrant {
        scope: RunGrantScope {
            chapter_id: proposal.chapter_id, page_indices: proposal.page_indices,
            #[cfg(not(test))]
            page_sources: proposal.page_sources.clone(),
            capabilities: proposal.capabilities, provider: proposal.provider,
            profile_id: proposal.profile_id, endpoint_fingerprint: stored.endpoint_fingerprint,
            profile_epoch: stored.profile_epoch, models: proposal.models,
        },
        expires_at_ms: grant.expires_at_ms,
        #[cfg(test)]
        page_sources: proposal.page_sources,
    });
    Ok(grant)
}

/// [`confirm_run`] for a proposal in `library`'s projects. A proposal the
/// project's standing consent covers is confirmed with the statements that
/// consent was given with, so the interface never answers them for the user.
/// Only an answer that ticked both is recorded to stand.
pub(crate) fn confirm_run_in_project(library: Option<&Library>, proposal_id: &str, rights_attested: bool,
    retention_acknowledged: bool) -> Result<RunGrant, String> {
    let destination = proposal_destination(proposal_id);
    let standing = match (&destination, library) {
        (Some((chapter_id, provider, profile_id, endpoint)), Some(library)) =>
            library.cloud_consent_covers(chapter_id, *provider, profile_id, endpoint),
        _ => false,
    };
    let grant = confirm_run(proposal_id, rights_attested || standing, retention_acknowledged || standing)?;
    // The first consent in a project stands for the rest of it.
    if let (Some((chapter_id, provider, profile_id, endpoint)), Some(library)) = (destination, library) {
        library.record_cloud_consent(&chapter_id, provider, &profile_id, &endpoint,
            rights_attested && retention_acknowledged);
    }
    Ok(grant)
}

/// Throw an unconfirmed proposal away. `false` when there was none.
pub(crate) fn discard_run_proposal(proposal_id: &str) -> Result<bool, String> {
    Ok(proposals().lock().map_err(|e| e.to_string())?.remove(proposal_id).is_some())
}

/// The provider and profile an open grant was given for, read without spending it.
pub(crate) fn grant_profile(grant_id: &str) -> Option<(CloudProvider, String)> {
    grants().lock().ok()?.get(grant_id).map(|stored| (stored.scope.provider, stored.scope.profile_id.clone()))
}

/// Spend a run grant. It is gone after the first presentation, matched or not,
/// so a refused grant cannot be tried again with other arguments.
pub(crate) fn consume_grant(grant_id: &str, request: &RunGrantRequest<'_>) -> Result<RunGrantScope, String> {
    consume_grant_at(grant_id, request, analysis::now_ms())
}

fn consume_grant_at(grant_id: &str, request: &RunGrantRequest<'_>, now_ms: u64) -> Result<RunGrantScope, String> {
    let stored = grants().lock().map_err(|e| e.to_string())?
        .remove(grant_id).ok_or("cloud_run_grant_missing")?;
    if now_ms >= stored.expires_at_ms { return Err("cloud_run_grant_expired".into()); }
    let scope = stored.scope;
    if scope.chapter_id != request.chapter_id { return Err("cloud_run_grant_mismatch: chapter".into()); }
    if request.pages.is_empty() || request.pages.iter().any(|page| !scope.page_indices.contains(page)) {
        return Err("cloud_run_grant_mismatch: pages".into());
    }
    if scope.capabilities != request.capabilities { return Err("cloud_run_grant_mismatch: capabilities".into()); }
    if scope.provider != request.provider || scope.profile_id != request.profile_id {
        return Err("cloud_run_grant_mismatch: profile".into());
    }
    if scope.endpoint_fingerprint != request.endpoint_fingerprint {
        return Err("cloud_run_grant_mismatch: endpoint".into());
    }
    if scope.profile_epoch != request.profile_epoch { return Err("cloud_run_profile_changed".into()); }
    #[cfg(test)]
    TEST_CONSUMED_PAGES.with(|pages| *pages.borrow_mut() = Some((scope.clone(), stored.page_sources)));
    Ok(scope)
}

/* ------------------------------------------------------------------ */
/* The run's cloud stage                                               */
/* ------------------------------------------------------------------ */

/// One page's cloud answer, in page coordinates.
#[derive(Debug, Clone)]
pub struct RemotePage {
    pub width: u32,
    pub height: u32,
    pub mask: Option<Vec<u8>>,
    pub boxes: Vec<BalloonBox>,
}

/// What a run holds for its cloud stages: the client, the spent grant's scope,
/// and the run's cancel flag.
pub struct RemoteDetection {
    client: Box<dyn AnalysisGateway + Send>,
    scope: RunGrantScope,
    page_sources: Vec<GrantedPage>,
    job_path: Option<PathBuf>,
    journal: Option<AnalysisJournal>,
    cancel: Arc<AtomicBool>,
    /// Whether the selected endpoint is still active, asked before each tile.
    allowed: Box<dyn Fn() -> Result<(), &'static str> + Send>,
}

impl RemoteDetection {
    pub(crate) fn new(client: Box<dyn AnalysisGateway + Send>, scope: RunGrantScope) -> Self {
        #[cfg(not(test))]
        let page_sources = scope.page_sources.clone();
        #[cfg(test)]
        let page_sources = TEST_CONSUMED_PAGES.with(|pages| {
            let mut pages = pages.borrow_mut();
            if pages.as_ref().is_some_and(|(bound, _)| *bound == scope) {
                pages.take().map(|(_, sources)| sources).unwrap_or_default()
            } else { Vec::new() }
        });
        Self { client, scope, page_sources, job_path: None, journal: None, cancel: Arc::new(AtomicBool::new(false)),
            allowed: Box::new(|| Ok(())) }
    }

    pub(crate) fn set_cancel(&mut self, cancel: Arc<AtomicBool>) { self.cancel = cancel; }

    pub(crate) fn set_allowed(&mut self, allowed: Box<dyn Fn() -> Result<(), &'static str> + Send>) {
        self.allowed = allowed;
    }

    fn tile_authority(&self) -> Result<(), String> {
        (self.allowed)().map_err(str::to_owned)?;
        if GrantService::global().get_profile_epoch(self.scope.provider, &self.scope.profile_id)
            != self.scope.profile_epoch {
            return Err("cloud_run_profile_changed".into());
        }
        Ok(())
    }

    /// RT-DETR v2 full boxes come from the cloud.
    pub fn rt_full(&self) -> bool { self.scope.capabilities.iter().any(|id| id == RT) }

    /// The SAM-TS-L mask comes from the cloud.
    pub fn sam(&self) -> bool { self.scope.capabilities.iter().any(|id| id == SAM) }

    /// Whether this run's grant covers the page at `page_index`.
    pub(crate) fn grants_page(&self, page_index: usize) -> bool {
        u32::try_from(page_index).is_ok_and(|index| self.scope.page_indices.contains(&index))
    }

    /// Send one granted page, tile by tile, to every consented model. A
    /// failure the endpoint's transport names is given its stable code.
    pub(crate) fn analyze_page(&self, page_index: usize, source: &[u8], page: &Raster)
        -> Result<RemotePage, String> {
        self.analyze_granted(page_index, source, page).map_err(coded)
    }

    fn analyze_granted(&self, page_index: usize, source: &[u8], page: &Raster)
        -> Result<RemotePage, String> {
        if !u32::try_from(page_index).is_ok_and(|index| self.scope.page_indices.contains(&index)) {
            return Err("cloud_run_page_not_granted".into());
        }
        self.tile_authority()?;
        if source.len() as u64 > MAX_SOURCE_BYTES {
            return Err("Source page exceeds the review-preview limit".into());
        }
        if u64::from(page.width) * u64::from(page.height) > MAX_PAGE_PIXELS {
            return Err("Analysis page exceeds the 24 megapixel review limit".into());
        }
        if self.cancel.load(Ordering::SeqCst) { return Err("analysis_cancelled".into()); }
        let available = self.client.capabilities()?;
        analysis::current_models(&available, &self.scope.models)?;
        let extent = cloud_tiles::plan(page.width, page.height, &[])?;
        let mut encoded = Vec::with_capacity(extent.tiles.len());
        for rect in &extent.tiles {
            let (sha, png) = analysis::encode_tile(page, *rect)?;
            if analysis::exceeds_limits(&available, *rect, Some(png.len())) {
                return Err(analysis::gateway_limit_error());
            }
            encoded.push((*rect, sha, png));
        }
        let source_sha256 = hex(source);
        #[cfg(test)]
        let unbound_test_page = self.page_sources.is_empty();
        #[cfg(not(test))]
        let unbound_test_page = false;
        if !unbound_test_page && !self.page_sources.iter().any(|bound|
            bound.page_index as usize == page_index && bound.source_sha256 == source_sha256) {
            return Err("cloud_run_source_changed".into());
        }
        if let Some(job_path) = &self.job_path {
            // The page walker holds this job's write lock for the whole chapter
            // while waiting for Prefetch::take. A producer waiting for that same
            // lock here deadlocks the first page. The manifest is replaced by
            // atomic rename, so a read-only snapshot is sufficient to verify
            // that the page still names the consented source index; Job::open
            // is unsuitable because it also sweeps detection sidecars.
            let manifest = std::fs::read(job_path).map_err(|_| "cloud_run_source_changed")?;
            let project: cleaner_core::project::Project = serde_json::from_slice(&manifest)
                .map_err(|_| "cloud_run_source_changed")?;
            if !(cleaner_core::project::OLDEST_READABLE_VERSION..=cleaner_core::project::FORMAT_VERSION)
                .contains(&project.version) {
                return Err("cloud_run_source_changed".into());
            }
            let current = Library::resolve_page(&project, page_index);
            if !self.page_sources.iter().any(|bound|
                bound.page_index as usize == page_index && current == Some(bound.source_idx)) {
                return Err("cloud_run_source_changed".into());
            }
        }
        let grants = GrantService::global();
        for model in &self.scope.models {
            let scope = page_scope(&self.scope, model, page_index, &source_sha256, page, &encoded);
            let grant = grants.issue_grant_with_epoch(scope.clone(), self.scope.profile_epoch,
                Duration::from_secs(60), 1).map_err(grant_error)?;
            grants.validate_and_consume(&grant.nonce, &scope).map_err(grant_error)?;
        }
        let uploads: Vec<TileUpload<'_>> = encoded.iter()
            .map(|(rect, sha, png)| (*rect, sha.as_str(), png.as_slice())).collect();
        let cancel = Arc::clone(&self.cancel);
        let record = if let Some(journal) = &self.journal {
            let proposal_id = analysis::random_id()?;
            let model_identity_sha256 = hex(&serde_json::to_vec(&self.scope.models)
                .map_err(|_| "analysis model identity unavailable")?);
            let underlay_sha256 = hex(encoded.iter().map(|(_, sha, _)| sha.as_str())
                .collect::<Vec<_>>().join(":").as_bytes());
            let record = AnalysisAttemptRecord {
                created_at_ms: analysis::now_ms(), schema_version: ANALYSIS_JOURNAL_SCHEMA_VERSION,
                proposal_id, provider: self.scope.provider, profile_id: self.scope.profile_id.clone(),
                capability: self.scope.capabilities.join("+"), source_sha256: source_sha256.clone(),
                underlay_sha256, model_identity_sha256: Some(model_identity_sha256),
                chapter_id: Some(self.scope.chapter_id.clone()), page_index: Some(page_index as u32),
                total_tiles: encoded.len() * self.scope.models.len(), completed_tiles: 0,
                reported_cost_usd: None, tile_submission_started: Some(false),
                cancel_requested: false, phase: AnalysisPhase::Confirmed,
            };
            journal.write(&record).map_err(|_| "analysis journal unavailable")?;
            Some(record)
        } else { None };
        let record = std::cell::RefCell::new(record);
        let cost = std::cell::Cell::new(0.0);
        let all_costs_known = std::cell::Cell::new(true);
        let sent = analysis::run_tiles(&*self.client, &self.scope.models,
            &source_sha256, &uploads, page.width, page.height, &cancel,
            &mut |index| {
                if cancel.load(Ordering::SeqCst) { return Err(analysis::AnalysisFlowError::Cancelled); }
                self.tile_authority().map_err(analysis::AnalysisFlowError::Other)?;
                if let (Some(journal), Some(rec)) = (&self.journal, record.borrow_mut().as_mut()) {
                    rec.phase = AnalysisPhase::SubmittedTile { index };
                    rec.tile_submission_started = Some(true);
                    if journal.write(rec).is_err() {
                        rec.phase = AnalysisPhase::Failed { code: "journal_unavailable".into() };
                        rec.tile_submission_started = Some(rec.completed_tiles > 0);
                        return Err(analysis::AnalysisFlowError::Other("analysis journal unavailable".into()));
                    }
                }
                Ok(())
            },
            &mut |index, result| {
                if let (Some(journal), Some(rec)) = (&self.journal, record.borrow_mut().as_mut()) {
                    journal.cache_tile_result(&rec.proposal_id, index, result)
                        .map_err(|_| "analysis result cache unavailable")?;
                    if let Some(value) = result.reported_cost_usd {
                        cost.set(cost.get() + value);
                    } else { all_costs_known.set(false); }
                    rec.completed_tiles = index + 1;
                    rec.reported_cost_usd = all_costs_known.get().then_some(cost.get());
                    rec.phase = AnalysisPhase::ResultCachedTile { index };
                    journal.write(rec).map_err(|_| "analysis journal unavailable")?;
                }
                if cancel.load(Ordering::SeqCst) {
                    return Err(analysis::AnalysisFlowError::Cancelled);
                }
                Ok(())
            });
        if let (Some(journal), Some(rec)) = (&self.journal, record.borrow_mut().as_mut()) {
            match &sent {
                Ok(_) => rec.phase = AnalysisPhase::RunPageComplete,
                Err(error) => {
                    rec.cancel_requested = cancel.load(Ordering::SeqCst);
                    rec.phase = if matches!(rec.phase, AnalysisPhase::SubmittedTile { .. }) {
                        // The request may have reached the provider without an
                        // answer. A previous tile's price is not a page price.
                        rec.reported_cost_usd = None;
                        AnalysisPhase::UnknownRemoteState { index: rec.completed_tiles }
                    } else if *error == analysis::AnalysisFlowError::Cancelled {
                        AnalysisPhase::Cancelled
                    } else {
                        AnalysisPhase::Failed { code: "analysis_failed".into() }
                    };
                }
            }
            journal.write(rec).map_err(|_| "analysis journal unavailable")?;
        }
        let Stitched { mask, boxes } = sent.map_err(|e| e.to_string())?;
        Ok(RemotePage { width: page.width, height: page.height, mask, boxes })
    }

    /// Ask the gateway to release the analysis GPU if it is idle, once this run
    /// has nothing more to send. Best effort and never the run's business: any
    /// failure (a gateway without `idle_only` answers 400, Beam has no route, the
    /// network is down) is logged and the GPU scales down on its idle timer, as it
    /// did before. Not sent at all once cloud engines are off or the profile has
    /// changed, because the run no longer speaks for that endpoint.
    pub(crate) fn release_idle(&self) {
        if (self.allowed)().is_err()
            || GrantService::global().get_profile_epoch(self.scope.provider, &self.scope.profile_id)
                != self.scope.profile_epoch {
            return;
        }
        if let Err(detail) = self.client.release_idle() {
            eprintln!("manga-cleaner: analysis GPU idle release skipped: {detail}");
        }
    }
}

/* ------------------------------------------------------------------ */
/* Ahead of the cleaning                                               */
/* ------------------------------------------------------------------ */

/// How many bytes of analyzed pages a run holds before its producer waits.
///
/// A mask is one byte per pixel, about 4 MB for an ordinary scan and 24 MB at
/// the page limit, and a run may have 256 pages, so an unbounded buffer could
/// reach a gigabyte on a chapter whose cleaning is slow. Bounded by bytes rather
/// than bit-packed, because bounding is correct whatever the mask holds: a
/// stitched mask is a level per pixel, not a flag, so packing it to bits would
/// lose what the local detector reads, and a packed buffer is still unbounded in
/// the page count. 256 MiB is some sixty ordinary pages ahead, most chapters
/// whole, which is all the lead the GPU needs to be done early.
pub const PREFETCH_BUDGET_BYTES: usize = 256 << 20;

/// One page for the producer, where the run will read it from. `None` when the
/// manifest names no source for it, which the run will fail on too.
pub(crate) struct PrefetchPage {
    pub page_index: usize,
    pub source: Option<PathBuf>,
    pub job_path: Option<PathBuf>,
}

/// One page's answer, as the run will take it.
struct Prefetched {
    page_index: usize,
    /// The bytes the producer analyzed, so the run can tell a page that changed
    /// on disk in between from the page it is cleaning.
    source_sha256: String,
    answer: Result<RemotePage, String>,
    bytes: usize,
}

#[derive(Default)]
struct Queue {
    ready: VecDeque<Prefetched>,
    bytes: usize,
    /// The producer has pushed its last page, or stopped.
    done: bool,
    /// The run is over; a producer waiting for room gives up.
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    changed: Condvar,
    budget: usize,
}

impl Shared {
    /// Poison is recovered: the queue is plain data and a panicking producer is
    /// already marked done by [`Finish`].
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Queue one page, waiting while the buffer is full. The buffer only ever
    /// exceeds the budget by holding one page that alone is larger. `false` when
    /// the run ended while this waited.
    fn push(&self, item: Prefetched) -> bool {
        let mut queue = self.lock();
        while !queue.closed && !queue.ready.is_empty() && queue.bytes + item.bytes > self.budget {
            queue = self.changed.wait(queue).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if queue.closed { return false; }
        queue.bytes += item.bytes;
        queue.ready.push_back(item);
        self.changed.notify_all();
        true
    }
}

/// Marks the producer done however it leaves, a panic included, so a run
/// waiting on a page is never left waiting on a thread that has gone.
struct Finish<'a>(&'a Shared);

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        self.0.lock().done = true;
        self.0.changed.notify_all();
    }
}

/// A run's cloud analysis, running ahead of its cleaning. See the module docs.
///
/// Owns the producer thread. Dropping it, which happens when the run's pipeline
/// goes at the end of the run however it ended, closes the queue and joins the
/// producer: it is then between pages, or finishing one tile the run's cancel
/// flag will stop, or sending its idle release, all bounded by the client's
/// timeouts. The run has already emitted `run-finished` and handed its slot back
/// by then, so the wait delays nobody.
pub struct Prefetch {
    shared: Arc<Shared>,
    /// The pages the producer was handed, so [`Prefetch::peek`] never waits
    /// for one it will not produce.
    pages: std::collections::HashSet<usize>,
    cancel: Arc<AtomicBool>,
    rt_full: bool,
    sam: bool,
    producer: Option<JoinHandle<()>>,
}

impl Prefetch {
    pub(crate) fn start(remote: RemoteDetection, pages: Vec<PrefetchPage>) -> Self {
        Self::start_with_budget(remote, pages, PREFETCH_BUDGET_BYTES)
    }

    pub(crate) fn start_with_budget(remote: RemoteDetection, pages: Vec<PrefetchPage>, budget: usize) -> Self {
        let shared = Arc::new(Shared { queue: Mutex::new(Queue::default()), changed: Condvar::new(), budget });
        let (cancel, rt_full, sam) = (Arc::clone(&remote.cancel), remote.rt_full(), remote.sam());
        let pages_handed = pages.iter().map(|page| page.page_index).collect();
        let producer = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || produce(&remote, pages, &shared))
        };
        Self { shared, pages: pages_handed, cancel, rt_full, sam, producer: Some(producer) }
    }

    /// RT-DETR v2 full boxes come from the cloud.
    pub fn rt_full(&self) -> bool { self.rt_full }

    /// The SAM-TS-L mask comes from the cloud.
    pub fn sam(&self) -> bool { self.sam }

    /// This page's cloud answer, waiting for the producer if cleaning has caught
    /// up with it. `source_sha256` is the page the run read.
    ///
    /// Pages ahead of it in the queue are ones the run skipped without asking,
    /// because it could not read their job or their file; they are dropped here.
    /// Both walk the same pages in the same order, so nothing the run will ask
    /// for is ever behind it.
    pub(crate) fn take(&self, page_index: usize, source_sha256: &str) -> Result<RemotePage, String> {
        let mut queue = self.shared.lock();
        loop {
            while let Some(item) = queue.ready.pop_front() {
                queue.bytes -= item.bytes;
                self.shared.changed.notify_all();
                if item.page_index != page_index { continue; }
                let page = item.answer?;
                if item.source_sha256 != source_sha256 {
                    return Err("cloud_run_source_changed: the page changed on disk after it was analyzed".into());
                }
                return Ok(page);
            }
            if queue.done {
                return Err(if self.cancel.load(Ordering::SeqCst) { "analysis_cancelled" }
                    else { "cloud_run_page_missing" }.into());
            }
            queue = self.shared.changed.wait(queue).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

impl Prefetch {
    /// A later page's answer without taking it: a long strip's detection
    /// window reads a few rows of the next page, and the walk takes that
    /// page's answer itself when it gets there. Waits for the producer as
    /// [`Prefetch::take`] does. `None` for a page it was not handed, one it
    /// failed or has not reached when it stopped, and one the walk has
    /// already taken.
    pub(crate) fn peek(&self, page_index: usize) -> Option<RemotePage> {
        if !self.pages.contains(&page_index) { return None; }
        let mut queue = self.shared.lock();
        loop {
            if let Some(item) = queue.ready.iter().find(|item| item.page_index == page_index) {
                return item.answer.as_ref().ok().cloned();
            }
            if queue.done { return None; }
            queue = self.shared.changed.wait(queue).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

impl Drop for Prefetch {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.changed.notify_all();
        if let Some(producer) = self.producer.take() {
            let _ = producer.join();
        }
    }
}

/// The displayed page, using the same patch selection as the tile renderer.
/// Hidden and deleted layers are excluded; empty masks change no pixels.
/// Keep `source` separate: cloud grants identify the original file.
pub(crate) fn page_input(job: &Job, page_index: usize, source: &[u8]) -> Result<Raster, String> {
    let raw = decode(source).map_err(|e| e.to_string())?;
    let raw = cleaner_core::image::orientation::from_bytes(source).raster(&raw);
    let source_idx = Library::resolve_page(&job.project, page_index).ok_or("no such source")?;
    let patches = crate::tile::visible_patches(job, page_index, source_idx).map_err(|e| e.to_string())?;
    cleaner_core::composite::composite(&raw, &patches).map_err(|e| e.to_string())
}

/// The producer: every page in order until the last, a cancel, a failure that
/// stops the run, or the run's end. Then, if it sent anything, the idle release,
/// after the queue is marked done so the run never waits on that call.
fn produce(remote: &RemoteDetection, pages: Vec<PrefetchPage>, shared: &Shared) {
    let mut sent = false;
    {
        let _finish = Finish(shared);
        for page in pages {
            if remote.cancel.load(Ordering::SeqCst) || shared.lock().closed { break; }
            let read = page.source.ok_or_else(|| "cloud_run_source_unreadable".to_owned())
                .and_then(|path| std::fs::read(&path).map_err(|_| "cloud_run_source_unreadable".to_owned()));
            let (source_sha256, answer) = match read {
                Ok(bytes) => {
                    let input = match &page.job_path {
                        Some(path) => Job::open(path).map_err(|e| e.to_string())
                            .and_then(|job| page_input(&job, page.page_index, &bytes)),
                        None => decode(&bytes).map(|raw| cleaner_core::image::orientation::from_bytes(&bytes).raster(&raw)).map_err(|e| e.to_string()),
                    };
                    let answer = input.and_then(|raster| {
                        sent = true;
                        remote.analyze_page(page.page_index, &bytes, &raster)
                    });
                    (sha256_hex(&bytes), answer)
                }
                Err(detail) => (String::new(), Err(detail)),
            };
            let stop = answer.as_ref().is_err_and(|detail| stops_run(detail));
            let bytes = answer.as_ref().map_or(0, |page| {
                page.mask.as_ref().map_or(0, Vec::len) + page.boxes.len() * std::mem::size_of::<BalloonBox>()
            });
            if !shared.push(Prefetched { page_index: page.page_index, source_sha256, answer, bytes }) || stop {
                break;
            }
        }
    }
    if sent { remote.release_idle(); }
}

/// The endpoint refused the key, or there is no usable key, in the stable
/// codes the rest of the cloud surface uses. Anything else is left as it is.
fn coded(detail: String) -> String {
    use crate::inference::http::HttpTransportError as Transport;
    let refused = [401u16, 403].map(|status| Transport::UnexpectedStatus { status }.to_string());
    if refused.contains(&detail) { return format!("gateway_unauthorized: {detail}"); }
    let keyless = [Transport::InvalidCredential.to_string(), Transport::CredentialProviderMismatch.to_string()];
    if keyless.contains(&detail) { return format!("credential_missing: {detail}"); }
    detail
}

/// Whether a page's cloud failure ends the run rather than only that page:
/// the run's authority is gone, so every later page would fail the same way.
pub(crate) fn stops_run(detail: &str) -> bool {
    let code = detail.split(':').next().unwrap_or("").trim();
    matches!(code, "cloud_run_profile_changed" | "cloud_disabled" | "gateway_unauthorized" | "credential_missing")
        || detail.starts_with("capability_unavailable: model identity changed")
}

fn grant_error(error: GrantError) -> String {
    match error {
        GrantError::ProfileMutated | GrantError::Revoked => "cloud_run_profile_changed".into(),
        other => other.to_string(),
    }
}

/// The per-page grant a run spends before each page's first tile.
fn page_scope(run: &RunGrantScope, model: &RemoteModel, page_index: usize, source_sha256: &str,
    page: &Raster, tiles: &[(TileRect, String, Vec<u8>)]) -> GrantScope {
    let tiles_digest = hex(tiles.iter().map(|(_, sha, _)| sha.as_str()).collect::<Vec<_>>().join(":").as_bytes());
    GrantScope {
        capability: model.capability.clone(), provider: run.provider,
        profile_id: run.profile_id.clone(),
        canonical_endpoint_fingerprint: run.endpoint_fingerprint.clone(),
        source_hash: source_sha256.to_string(),
        crop_sha256: tiles.first().map(|(_, sha, _)| sha.clone()).unwrap_or_else(|| tiles_digest.clone()),
        hint_sha256: tiles_digest.clone(),
        operation_digest: hex(format!("run:{}:{page_index}", run.chapter_id).as_bytes()),
        crop_bounds: Rect::new(0, 0, page.width, page.height),
        mask_hash: source_sha256.to_string(), revision: 0,
        input_sha256: tiles_digest,
        predecessors_sha256: String::new(),
        recipe: RenderRecipe::new(&model.capability, VERSION, &model.capability, &model.model_revision, false),
        region_ids: vec![format!("page_{page_index}")],
    }
}

/// Spend a run's grant and open its client. Called by `run_clean` before any
/// model is loaded; every refusal is typed and nothing has been sent.
pub(crate) fn open_for_run(
    app: &tauri::AppHandle,
    chapter_id: &str,
    pages: &[u32],
    capabilities: &[String],
    grant_id: Option<&str>,
) -> Result<RemoteDetection, String> {
    let grant_id = grant_id.ok_or("cloud_run_grant_required")?;
    if !crate::inference::cloud_allowed(app) { return Err("cloud_disabled".into()); }
    // The profile the grant was given for, not the one selected now: a run
    // queued behind another keeps its destination when the user picks another.
    let (provider, profile_id) = grant_profile(grant_id).ok_or("cloud_run_grant_missing")?;
    let (profile, client) = analysis::configured_profile(app, provider, &profile_id)?;
    let endpoint = crate::inference::config::compute_canonical_endpoint_fingerprint(&profile.endpoint_url)?;
    let scope = consume_grant(grant_id, &RunGrantRequest {
        chapter_id, pages, capabilities, provider, profile_id: &profile_id,
        endpoint_fingerprint: &endpoint,
        profile_epoch: GrantService::global().get_profile_epoch(provider, &profile_id),
    })?;
    let mut detection = RemoteDetection::new(Box::new(client), scope);
    detection.journal = Some(analysis::journal_for(app)?);
    detection.job_path = Some(Library::for_app(app).map_err(|_| "cloud_run_source_changed")?
        .resolve_chapter(chapter_id).map_err(|_| "cloud_run_source_changed")?);
    let app = app.clone();
    detection.set_allowed(Box::new(move || {
        crate::inference::profile_in_service(&app, provider, &profile_id).map_err(|lost| match lost {
            crate::inference::Withdrawn::CloudDisabled => "cloud_disabled",
            crate::inference::Withdrawn::ProfileGone => "cloud_run_profile_changed",
        })
    }));
    Ok(detection)
}

/// The pages a run of this scope would walk, as the run itself plans them.
/// `page` is the one page asked for; `chapter` every page of the chapter the
/// run would clean. A project has no single consent.
fn pages_for_scope(app: &tauri::AppHandle, scope: Option<&str>, chapter_id: &str,
    page_indices: Option<Vec<u32>>) -> Result<Vec<u32>, String> {
    let planned = |scope: &str, page: Option<u32>| -> Result<Vec<u32>, String> {
        let library = Library::for_app(app).map_err(|e| e.to_string())?;
        Ok(crate::run::plan_cloud_detection(&library, scope, chapter_id, page).map_err(|e| e.to_string())?
            .into_iter().map(|entry| entry.page_index as u32).collect())
    };
    match scope {
        None => Ok(page_indices.unwrap_or_default()),
        Some("page") => match page_indices.as_deref() {
            Some([page]) => planned("page", Some(*page)),
            _ => Err("cloud_run_no_pages".into()),
        },
        Some("chapter") => planned("chapter", None),
        Some(_) => Err("cloud_run_scope_unsupported".into()),
    }
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn propose_run_analysis(
    app: tauri::AppHandle, chapter_id: String, scope: Option<String>, page_indices: Option<Vec<u32>>,
    capabilities: Vec<String>, provider: CloudProvider, profile_id: String,
) -> Result<RunAnalysisProposal, String> {
    if !crate::inference::cloud_allowed(&app) { return Err("cloud_disabled".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let page_indices = pages_for_scope(&app, scope.as_deref(), &chapter_id, page_indices)?;
        let (selected, client) = analysis::profile(&app, provider, &profile_id)?;
        let library = Library::for_app(&app).map_err(|e| e.to_string())?;
        let path = library.resolve_chapter(&chapter_id).map_err(|e| e.to_string())?;
        let endpoint = crate::inference::config::compute_canonical_endpoint_fingerprint(&selected.endpoint_url)?;
        let mut proposal = propose_run(&path, chapter_id, page_indices, capabilities, provider, profile_id,
            selected.name, endpoint.clone(), &client)?;
        proposal.standing = library.cloud_consent_covers(&proposal.chapter_id, provider,
            &proposal.profile_id, &endpoint);
        Ok(proposal)
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn confirm_run_analysis(
    app: tauri::AppHandle, proposal_id: String, rights_attested: bool, retention_acknowledged: bool,
) -> Result<RunGrant, String> {
    if !crate::inference::cloud_allowed(&app) { return Err("cloud_disabled".into()); }
    let library = Library::for_app(&app).ok();
    confirm_run_in_project(library.as_ref(), &proposal_id, rights_attested, retention_acknowledged)
}

#[tauri::command]
pub fn cancel_run_analysis(proposal_id: String) -> Result<bool, String> {
    discard_run_proposal(&proposal_id)
}

/// A gateway for tests that answers every tile, and records what it was sent.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::AtomicUsize;
    use cleaner_core::cloud_analysis_wire::{
        AnalysisBox, AnalysisCapabilities, AnalysisCapability, AnalysisLimits, AnalysisRequest, AnalysisResult,
        AnalysisTimings,
    };
    use base64::Engine as _;

    #[derive(Clone)]
    pub(crate) struct Gateway {
        pub sent: Arc<AtomicUsize>,
        pub revision: String,
        /// Every tile's source page, in the order the tiles arrived.
        pub seen: Arc<Mutex<Vec<String>>>,
        pub uploads: Arc<Mutex<Vec<Vec<u8>>>>,
        /// Source pages whose tiles fail, with the transport's words for it.
        pub fail: Arc<Mutex<HashMap<String, String>>>,
        /// Idle releases asked for.
        pub releases: Arc<AtomicUsize>,
        /// While set, a tile waits here until its source page is let through.
        pub hold: Option<Arc<(Mutex<HashSet<String>>, Condvar)>>,
    }

    pub(crate) fn gateway() -> Gateway {
        Gateway {
            sent: Arc::new(AtomicUsize::new(0)), revision: "c".repeat(40),
            seen: Arc::default(), uploads: Arc::default(), fail: Arc::default(), releases: Arc::default(), hold: None,
        }
    }

    impl Gateway {
        /// Let one held page's tiles through.
        pub(crate) fn release_page(&self, source_sha256: &str) {
            let (open, changed) = &**self.hold.as_ref().expect("a held gateway");
            open.lock().unwrap().insert(source_sha256.to_owned());
            changed.notify_all();
        }
    }

    impl AnalysisGateway for Gateway {
        fn capabilities(&self) -> Result<AnalysisCapabilities, String> {
            Ok(AnalysisCapabilities {
                protocol_version: VERSION.into(),
                capabilities: vec![
                    AnalysisCapability { capability: SAM.into(),
                        graph_sha256s: vec!["a".repeat(64), "b".repeat(64)], model_revision: self.revision.clone() },
                    AnalysisCapability { capability: RT.into(),
                        graph_sha256s: vec!["d".repeat(64)], model_revision: "e".repeat(40) },
                ],
                limits: AnalysisLimits { max_tile_side: 1024, max_tile_pixels: 1_048_576,
                    max_png_bytes: 4_194_304, max_components: 4096, max_boxes: 4096 },
            })
        }

        fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String> {
            request.validate(png)?;
            if let Some(hold) = &self.hold {
                let (open, changed) = &**hold;
                let mut open = open.lock().unwrap();
                while !open.contains(&request.source_page_sha256) {
                    open = changed.wait(open).unwrap();
                }
            }
            self.seen.lock().unwrap().push(request.source_page_sha256.clone());
            self.uploads.lock().unwrap().push(png.to_vec());
            if let Some(error) = self.fail.lock().unwrap().get(&request.source_page_sha256) {
                return Err(error.clone());
            }
            self.sent.fetch_add(1, Ordering::SeqCst);
            let rect = request.tile_rect;
            let sam = request.capability == SAM;
            let mask = sam.then(|| {
                let mut bytes = Vec::new();
                {
                    let mut encoder = png::Encoder::new(&mut bytes, rect.width, rect.height);
                    encoder.set_color(png::ColorType::Grayscale);
                    encoder.set_depth(png::BitDepth::Eight);
                    encoder.write_header().unwrap()
                        .write_image_data(&vec![255; (rect.width * rect.height) as usize]).unwrap();
                }
                base64::engine::general_purpose::STANDARD.encode(bytes)
            });
            let result = AnalysisResult {
                protocol_version: VERSION.into(), capability: request.capability.clone(),
                request_digest: request.request_digest.clone(), tile_id: request.tile_id.clone(),
                tile_rect: rect, graph_sha256s: request.graph_sha256s.clone(),
                model_revision: request.model_revision.clone(), mask_png_b64: mask,
                components: vec![],
                boxes: if sam { vec![] } else {
                    vec![AnalysisBox { rect: TileRect { x: 2, y: 3, width: 10, height: 8 }, class: 2, score: 0.9 }]
                },
                timings: AnalysisTimings { load_ms: 0, preprocess_ms: 0, inference_ms: 0, postprocess_ms: 0 },
                reported_cost_usd: None,
            };
            result.validate(request)?;
            Ok(result)
        }

        fn release_idle(&self) -> Result<(), String> {
            self.releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The models a run grant pins, as this gateway advertises them.
    pub(crate) fn models(capabilities: &[&str]) -> Vec<RemoteModel> {
        let advertised = gateway().capabilities().unwrap();
        capabilities.iter().map(|id| {
            let entry = advertised.capabilities.iter().find(|entry| entry.capability == *id).unwrap();
            RemoteModel { capability: entry.capability.clone(), graph_sha256s: entry.graph_sha256s.clone(),
                model_revision: entry.model_revision.clone() }
        }).collect()
    }

    /// A spent run grant for `pages`, as `consume_grant` would answer it, against
    /// a profile of the test's own: the epoch is process-wide.
    pub(crate) fn scope(profile_id: &str, pages: &[u32], capabilities: &[&str]) -> RunGrantScope {
        RunGrantScope {
            chapter_id: "chapter".into(), page_indices: pages.to_vec(),
            capabilities: capabilities.iter().map(|id| id.to_string()).collect(),
            provider: CloudProvider::Modal, profile_id: profile_id.into(),
            endpoint_fingerprint: "f".repeat(64),
            profile_epoch: GrantService::global().get_profile_epoch(CloudProvider::Modal, profile_id),
            models: models(capabilities),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::fake::{gateway, Gateway};
    use std::path::PathBuf;
    use cleaner_core::cloud_analysis_wire::{AnalysisCapabilities, AnalysisRequest, AnalysisResult};
    use cleaner_core::image::{fixtures, Format};
    use cleaner_core::project::{Project, StripMode};


    fn chapter(name: &str, pages: usize, mode: StripMode) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mc-run-analysis-{name}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let mut references = Vec::new();
        for index in 0..pages {
            let mut page = fixtures::by_name("l8").raster;
            page.width = 64;
            page.height = 48;
            page.data = vec![200; 64 * 48];
            let source = root.join(format!("page{index}.png"));
            let bytes = cleaner_core::image::encode(&page, Format::Png).unwrap();
            std::fs::write(&source, &bytes).unwrap();
            references.push(cleaner_core::ingest::source_ref(&source, &bytes).unwrap());
        }
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(manifest.parent().unwrap(), "test-app", mode, &references);
        Job::create(&manifest, project).unwrap();
        manifest
    }

    #[test]
    fn cloud_page_input_uses_oriented_geometry_before_compositing() {
        let root = std::env::temp_dir().join(format!("cloud-oriented-input-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../crates/cleaner-core/tests/fixtures/color-reference/orientation-6.jpg")).unwrap();
        let path = root.join("page.jpg"); std::fs::write(&path, &source).unwrap();
        let reference = cleaner_core::ingest::source_ref(&path, &source).unwrap();
        let job = Job::create(&root.join("chapter.mtclean"), Project::new(&root, "test", StripMode::Single, &[reference])).unwrap();
        let raw = decode(&source).unwrap();
        let actual = page_input(&job, 0, &source).unwrap();
        let expected = cleaner_core::image::orientation::Orientation(6).raster(&raw);
        assert_eq!((actual.width, actual.height), (raw.height, raw.width));
        assert_eq!(actual.data, expected.data);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn proposal(path: &Path, pages: Vec<u32>, capabilities: Vec<&str>, gateway: &Gateway) -> RunAnalysisProposal {
        propose_run(path, "chapter".into(), pages, capabilities.into_iter().map(String::from).collect(),
            CloudProvider::Modal, "modal-prof-run".into(), "Modal".into(), "f".repeat(64), gateway).unwrap()
    }

    fn request<'a>(pages: &'a [u32], capabilities: &'a [String]) -> RunGrantRequest<'a> {
        RunGrantRequest {
            chapter_id: "chapter", pages, capabilities, provider: CloudProvider::Modal,
            profile_id: "modal-prof-run", endpoint_fingerprint: &FINGERPRINT,
            profile_epoch: GrantService::global().get_profile_epoch(CloudProvider::Modal, "modal-prof-run"),
        }
    }

    static FINGERPRINT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| "f".repeat(64));

    fn both() -> Vec<String> { vec![SAM.to_string(), RT.to_string()] }

    #[test]
    fn targets_are_strict_and_default_local() {
        assert_eq!(AnalysisTargets::from_value(None).unwrap(), AnalysisTargets::default());
        for bad in [serde_json::json!("cloud"), serde_json::json!({"ctd": "cloud"}),
            serde_json::json!({"samTs": "gpu"}), serde_json::json!({"rtFull": true})] {
            assert!(AnalysisTargets::from_value(Some(&bad)).is_err(), "{bad}");
            assert!(AnalysisTargets::from_stored(Some(&bad)).is_err(), "{bad}");
        }
        let targets = AnalysisTargets { rt_full: true, sam: true };
        assert_eq!(AnalysisTargets::from_value(Some(&targets.to_value())).unwrap(), targets);
        assert_eq!(AnalysisTargets::from_stored(Some(&targets.to_value())).unwrap(), targets);
    }

    /// Detection runs in one place. A split is refused where it is written or
    /// passed to a run, so no cloud-capable stage runs here beside a cloud one
    /// unannounced; one an older build stored settles on this computer.
    #[test]
    fn split_targets_are_refused_and_a_stored_split_settles_local() {
        for split in [serde_json::json!({"rtFull": "cloud", "samTs": "local"}),
            serde_json::json!({"rtFull": "local", "samTs": "cloud"}),
            serde_json::json!({"samTs": "cloud"})] {
            let refused = AnalysisTargets::from_value(Some(&split)).unwrap_err();
            assert!(refused.contains("same place"), "{refused}");
            assert_eq!(AnalysisTargets::from_stored(Some(&split)).unwrap(), AnalysisTargets::default(), "{split}");
        }
    }

    #[test]
    fn only_selected_cloud_stages_become_capabilities() {
        let cloud = AnalysisTargets { rt_full: true, sam: true };
        assert_eq!(cloud.capabilities(true, true), vec![SAM.to_string(), RT.to_string()]);
        assert_eq!(cloud.capabilities(false, true), vec![SAM.to_string()]);
        assert!(cloud.capabilities(false, false).is_empty());
        assert!(AnalysisTargets::default().capabilities(true, true).is_empty());
    }

    #[test]
    fn proposal_states_pages_tiles_and_models_without_sending() {
        let path = chapter("propose", 3, StripMode::Single);
        let gateway = gateway();
        let proposal = proposal(&path, vec![2, 0, 2], vec![RT, SAM], &gateway);
        assert_eq!(proposal.page_indices, vec![0, 2]);
        assert_eq!(proposal.pages, 2);
        assert_eq!(proposal.total_tiles, 2);
        assert_eq!(proposal.total_tile_pixels, 2 * 64 * 48);
        assert_eq!(proposal.capabilities, both());
        assert_eq!(proposal.models.len(), 2);
        assert_eq!(gateway.sent.load(Ordering::SeqCst), 0);
        assert!(propose_run(&path, "chapter".into(), vec![0], vec!["coo@1".into()], CloudProvider::Modal,
            "modal-prof-run".into(), "Modal".into(), "f".repeat(64), &gateway).is_err());
        assert!(propose_run(&path, "chapter".into(), vec![9], vec![SAM.into()], CloudProvider::Modal,
            "modal-prof-run".into(), "Modal".into(), "f".repeat(64), &gateway).is_err());
    }

    #[test]
    fn long_strip_chapters_are_proposed_page_by_page() {
        let path = chapter("strip", 2, StripMode::Longstrip);
        let gateway = gateway();
        let proposal = proposal(&path, vec![0, 1], vec![SAM], &gateway);
        assert_eq!(proposal.page_indices, vec![0, 1]);
        assert_eq!(proposal.total_tiles, 2);
        assert_eq!(gateway.sent.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn consent_answers_are_required_and_a_proposal_is_used_once() {
        let path = chapter("consent", 1, StripMode::Single);
        let first = proposal(&path, vec![0], vec![SAM], &gateway());
        assert_eq!(confirm_run(&first.proposal_id, false, true).unwrap_err(), "rights_attestation_required");
        assert_eq!(confirm_run(&first.proposal_id, true, false).unwrap_err(), "retention_acknowledgement_required");
        let grant = confirm_run(&first.proposal_id, true, true).unwrap();
        assert_eq!(grant.capabilities, vec![SAM.to_string()]);
        assert_eq!(confirm_run(&first.proposal_id, true, true).unwrap_err(), "analysis_proposal_missing");
        let second = proposal(&path, vec![0], vec![SAM], &gateway());
        proposals().lock().unwrap().get_mut(&second.proposal_id).unwrap().proposal.expires_at_ms = 0;
        assert_eq!(confirm_run(&second.proposal_id, true, true).unwrap_err(), "analysis_proposal_expired");
    }

    #[test]
    fn a_standing_project_confirms_without_the_statements_only_when_it_holds_them() {
        let path = chapter("standing", 1, StripMode::Single);
        let scratch = std::env::temp_dir().join(format!("mc-run-analysis-standing-lib-{}-{}", std::process::id(),
            analysis::now_ms()));
        let library = Library::at(&scratch);
        let project = library.create_project("One", StripMode::Single, None, None).unwrap();
        let chapter_id = library.create_chapter(&project.id, "A", None, None).unwrap().created().unwrap().id;
        let propose = || propose_run(&path, chapter_id.clone(), vec![0], vec![SAM.into()], CloudProvider::Modal,
            "modal-prof-run".into(), "Modal".into(), "f".repeat(64), &gateway()).unwrap().proposal_id;
        let confirm = |id: &str, statements| confirm_run_in_project(Some(&library), id, statements, statements);
        // A refused answer leaves its proposal open; drop it, as a cancel does.
        let refused = |statements| {
            let id = propose();
            let error = confirm(&id, statements).unwrap_err();
            assert!(discard_run_proposal(&id).unwrap());
            error
        };

        assert_eq!(refused(false), "rights_attestation_required");
        // A consent without the statements, as recorded before they were asked.
        library.set_cloud_consent(&chapter_id, Some(crate::library::CloudConsent {
            provider: CloudProvider::Modal, profile_id: "modal-prof-run".into(), origin_fingerprint: "f".repeat(64),
            granted_at: 0, statements_accepted: false,
        })).unwrap();
        assert_eq!(refused(false), "rights_attestation_required");
        confirm(&propose(), true).unwrap();
        assert!(library.cloud_consent_covers(&chapter_id, CloudProvider::Modal, "modal-prof-run", &"f".repeat(64)));
        confirm(&propose(), false).expect("the project's statements stand for it");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn grant_refuses_every_mismatch_and_is_single_use() {
        let path = chapter("grant", 3, StripMode::Single);
        let gateway = gateway();
        let caps = both();
        let pages = [0u32, 1];
        let mint = || confirm_run(&proposal(&path, vec![0, 1], vec![SAM, RT], &gateway).proposal_id, true, true)
            .unwrap().grant_id;

        let ok = mint();
        let scope = consume_grant(&ok, &request(&pages, &caps)).unwrap();
        assert_eq!(scope.page_indices, vec![0, 1]);
        assert_eq!(consume_grant(&ok, &request(&pages, &caps)).unwrap_err(), "cloud_run_grant_missing");

        let subset = mint();
        assert!(consume_grant(&subset, &request(&[1], &caps)).is_ok());

        let wrong_chapter = mint();
        let mut asked = request(&pages, &caps);
        asked.chapter_id = "other";
        assert_eq!(consume_grant(&wrong_chapter, &asked).unwrap_err(), "cloud_run_grant_mismatch: chapter");
        // Refused once is spent: the right arguments cannot follow.
        assert_eq!(consume_grant(&wrong_chapter, &request(&pages, &caps)).unwrap_err(), "cloud_run_grant_missing");

        let wrong_pages = mint();
        assert_eq!(consume_grant(&wrong_pages, &request(&[0, 2], &caps)).unwrap_err(),
            "cloud_run_grant_mismatch: pages");

        let wrong_caps = mint();
        let sam_only = vec![SAM.to_string()];
        assert_eq!(consume_grant(&wrong_caps, &request(&pages, &sam_only)).unwrap_err(),
            "cloud_run_grant_mismatch: capabilities");

        let wrong_profile = mint();
        let mut asked = request(&pages, &caps);
        asked.profile_id = "modal-prof-other";
        assert_eq!(consume_grant(&wrong_profile, &asked).unwrap_err(), "cloud_run_grant_mismatch: profile");

        let wrong_provider = mint();
        let mut asked = request(&pages, &caps);
        asked.provider = CloudProvider::Beam;
        assert_eq!(consume_grant(&wrong_provider, &asked).unwrap_err(), "cloud_run_grant_mismatch: profile");

        let wrong_endpoint = mint();
        let other = "0".repeat(64);
        let mut asked = request(&pages, &caps);
        asked.endpoint_fingerprint = &other;
        assert_eq!(consume_grant(&wrong_endpoint, &asked).unwrap_err(), "cloud_run_grant_mismatch: endpoint");

        let stale_epoch = mint();
        let mut asked = request(&pages, &caps);
        asked.profile_epoch += 1;
        assert_eq!(consume_grant(&stale_epoch, &asked).unwrap_err(), "cloud_run_profile_changed");

        let expired = mint();
        grants().lock().unwrap().get_mut(&expired).unwrap().expires_at_ms = 0;
        assert_eq!(consume_grant(&expired, &request(&pages, &caps)).unwrap_err(), "cloud_run_grant_expired");

        assert_eq!(consume_grant("never-issued", &request(&pages, &caps)).unwrap_err(), "cloud_run_grant_missing");
    }

    #[test]
    fn a_granted_page_returns_boxes_and_mask_and_other_pages_send_nothing() {
        let path = chapter("page", 2, StripMode::Single);
        let gateway = gateway();
        let sent = Arc::clone(&gateway.sent);
        let grant = confirm_run(&proposal(&path, vec![0], vec![SAM, RT], &gateway).proposal_id, true, true).unwrap();
        let caps = both();
        let scope = consume_grant(&grant.grant_id, &request(&[0], &caps)).unwrap();
        let remote = RemoteDetection::new(Box::new(gateway), scope);
        assert!(remote.rt_full() && remote.sam());
        let mut raster = fixtures::by_name("l8").raster;
        raster.width = 64;
        raster.height = 48;
        raster.data = vec![200; 64 * 48];
        let source = cleaner_core::image::encode(&raster, Format::Png).unwrap();

        assert_eq!(remote.analyze_page(1, &source, &raster).unwrap_err(), "cloud_run_page_not_granted");
        assert_eq!(sent.load(Ordering::SeqCst), 0);

        let page = remote.analyze_page(0, &source, &raster).unwrap();
        assert_eq!(sent.load(Ordering::SeqCst), 2, "one tile, two models");
        assert_eq!(page.mask.as_ref().unwrap().len(), 64 * 48);
        assert!(page.mask.as_ref().unwrap().iter().all(|value| *value == 255));
        assert_eq!(page.boxes.len(), 1);
        assert_eq!(page.boxes[0].rect, Rect::new(2, 3, 10, 8));
    }

    #[test]
    fn cloud_run_page_journals_one_unpriced_attempt() {
        let path = chapter("usage-dispatch", 1, StripMode::Single);
        let root = path.parent().unwrap().parent().unwrap().join("usage-journal");
        let gateway = gateway();
        let sent = Arc::clone(&gateway.sent);
        let granted = confirm_run(&proposal(&path, vec![0], vec![SAM, RT], &gateway).proposal_id,
            true, true).unwrap();
        let capabilities = both();
        let scope = consume_grant(&granted.grant_id, &request(&[0], &capabilities)).unwrap();
        let mut remote = RemoteDetection::new(Box::new(gateway), scope);
        remote.journal = Some(AnalysisJournal::new(root.clone()));
        let (raster, source) = page_raster();
        remote.analyze_page(0, &source, &raster).unwrap();
        assert_eq!(sent.load(Ordering::SeqCst), 2);

        let entries = std::fs::read_dir(root.join("analysis")).unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".json") && !name.ends_with(".result.json"))
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1, "both models belong to one page attempt");
        let id = entries[0].trim_end_matches(".json");
        let record = remote.journal.as_ref().unwrap().read(id).unwrap();
        assert!(matches!(record.phase, AnalysisPhase::RunPageComplete));
        assert_eq!(record.reported_cost_usd, None, "provider omitted its price");
        assert_eq!(record.completed_tiles, 2);
        assert_eq!(record.tile_submission_started, Some(true));
        assert_eq!(record.source_sha256, hex(&source));
        assert_eq!(record.chapter_id.as_deref(), Some("chapter"));
        assert_eq!(record.page_index, Some(0));
        assert_eq!(record.model_identity_sha256.as_ref().map(String::len), Some(64));
    }

    #[test]
    fn cloud_run_preflight_failure_writes_no_billable_analysis_attempt() {
        let path = chapter("usage-preflight", 1, StripMode::Single);
        let root = path.parent().unwrap().parent().unwrap().join("usage-journal");
        let gateway = gateway();
        let sent = Arc::clone(&gateway.sent);
        let granted = confirm_run(&proposal(&path, vec![0], vec![SAM], &gateway).proposal_id,
            true, true).unwrap();
        let capabilities = vec![SAM.to_string()];
        let scope = consume_grant(&granted.grant_id, &request(&[0], &capabilities)).unwrap();
        let mut remote = RemoteDetection::new(Box::new(gateway), scope);
        remote.journal = Some(AnalysisJournal::new(root.clone()));
        let (mut raster, _) = page_raster();
        raster.data[0] = 17;
        let changed = cleaner_core::image::encode(&raster, Format::Png).unwrap();
        assert_eq!(remote.analyze_page(0, &changed, &raster).unwrap_err(), "cloud_run_source_changed");
        assert_eq!(sent.load(Ordering::SeqCst), 0);
        assert!(!root.join("analysis").exists());
    }

    #[test]
    fn cloud_run_transport_failure_keeps_unknown_dispatch_unpriced() {
        let path = chapter("usage-transport", 1, StripMode::Single);
        let root = path.parent().unwrap().parent().unwrap().join("usage-journal");
        let gateway = gateway();
        let (raster, source) = page_raster();
        gateway.fail.lock().unwrap().insert(hex(&source), "transport_lost".into());
        let granted = confirm_run(&proposal(&path, vec![0], vec![SAM], &gateway).proposal_id,
            true, true).unwrap();
        let capabilities = vec![SAM.to_string()];
        let scope = consume_grant(&granted.grant_id, &request(&[0], &capabilities)).unwrap();
        let mut remote = RemoteDetection::new(Box::new(gateway), scope);
        remote.journal = Some(AnalysisJournal::new(root.clone()));
        assert!(remote.analyze_page(0, &source, &raster).is_err());
        let entry = std::fs::read_dir(root.join("analysis")).unwrap().next().unwrap().unwrap();
        let id = entry.file_name().to_string_lossy().trim_end_matches(".json").to_owned();
        let record = remote.journal.as_ref().unwrap().read(&id).unwrap();
        assert!(matches!(record.phase, AnalysisPhase::UnknownRemoteState { index: 0 }));
        assert_eq!(record.tile_submission_started, Some(true));
        assert_eq!(record.reported_cost_usd, None);
    }

    struct CancelAfterResponse {
        gateway: Gateway,
        cancel: Arc<AtomicBool>,
    }

    impl AnalysisGateway for CancelAfterResponse {
        fn capabilities(&self) -> Result<AnalysisCapabilities, String> { self.gateway.capabilities() }
        fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String> {
            let mut result = self.gateway.analyze(request, png)?;
            result.reported_cost_usd = Some(0.25);
            self.cancel.store(true, Ordering::SeqCst);
            Ok(result)
        }
    }

    #[test]
    fn cancellation_after_last_response_caches_cost_without_yielding_page() {
        let path = chapter("usage-cancel-last", 1, StripMode::Single);
        let root = path.parent().unwrap().parent().unwrap().join("usage-journal");
        let gateway = gateway();
        let sent = Arc::clone(&gateway.sent);
        let granted = confirm_run(&proposal(&path, vec![0], vec![SAM], &gateway).proposal_id,
            true, true).unwrap();
        let capabilities = vec![SAM.to_string()];
        let scope = consume_grant(&granted.grant_id, &request(&[0], &capabilities)).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let client = CancelAfterResponse { gateway, cancel: Arc::clone(&cancel) };
        let mut remote = RemoteDetection::new(Box::new(client), scope);
        remote.journal = Some(AnalysisJournal::new(root.clone()));
        remote.set_cancel(cancel);
        let (raster, source) = page_raster();
        assert_eq!(remote.analyze_page(0, &source, &raster).unwrap_err(), "analysis_cancelled");
        assert_eq!(sent.load(Ordering::SeqCst), 1);
        let entries = std::fs::read_dir(root.join("analysis")).unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 2, "attempt and cached tile result");
        let id = entries.iter().find(|name| !name.ends_with(".result.json")).unwrap().trim_end_matches(".json");
        let record = remote.journal.as_ref().unwrap().read(id).unwrap();
        assert!(matches!(record.phase, AnalysisPhase::Cancelled));
        assert_eq!(record.completed_tiles, 1);
        assert_eq!(record.reported_cost_usd, Some(0.25));
        assert!(record.cancel_requested);
    }

    #[test]
    fn run_grant_rejects_a_changed_page_before_its_first_tile() {
        let path = chapter("changed-source", 1, StripMode::Single);
        let gateway = gateway();
        let sent = Arc::clone(&gateway.sent);
        let proposed = proposal(&path, vec![0], vec![SAM], &gateway);
        let grant = confirm_run(&proposed.proposal_id, true, true).unwrap();
        let caps = vec![SAM.to_string()];
        let scope = consume_grant(&grant.grant_id, &request(&[0], &caps)).unwrap();
        let remote = RemoteDetection::new(Box::new(gateway), scope);
        let (mut raster, _) = page_raster();
        raster.data[0] = 17;
        let changed = cleaner_core::image::encode(&raster, Format::Png).unwrap();
        assert_eq!(remote.analyze_page(0, &changed, &raster).unwrap_err(), "cloud_run_source_changed");
        assert_eq!(sent.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn run_grant_rejects_a_reordered_page_even_when_pixels_match() {
        let path = chapter("reordered-source", 2, StripMode::Single);
        let gateway = gateway();
        let sent = Arc::clone(&gateway.sent);
        let proposed = proposal(&path, vec![0], vec![SAM], &gateway);
        let grant = confirm_run(&proposed.proposal_id, true, true).unwrap();
        let caps = vec![SAM.to_string()];
        let scope = consume_grant(&grant.grant_id, &request(&[0], &caps)).unwrap();
        let mut remote = RemoteDetection::new(Box::new(gateway), scope);
        remote.job_path = Some(path.clone());
        let mut job = Job::open(&path).unwrap();
        job.project.strip.order.swap(0, 1);
        job.flush().unwrap();
        let (raster, source) = page_raster();
        assert_eq!(remote.analyze_page(0, &source, &raster).unwrap_err(), "cloud_run_source_changed");
        assert_eq!(sent.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cloud_page_analysis_does_not_wait_for_the_walkers_job_lock() {
        let path = chapter("prefetch-walker-lock", 1, StripMode::Single);
        let job = Job::open(&path).unwrap();
        let source_path = job.source_path(0).unwrap();
        let source = std::fs::read(source_path).unwrap();
        let raster = decode(&source).unwrap();
        let gateway = gateway();
        let sent = Arc::clone(&gateway.sent);
        let mut remote = RemoteDetection::new(Box::new(gateway),
            fake::scope("modal-prof-prefetch-walker-lock", &[0], &[SAM]));
        remote.job_path = Some(path.clone());
        remote.page_sources = vec![GrantedPage {
            page_index: 0, source_idx: 0, source_sha256: hex(&source),
        }];
        let walker_lock = crate::run::lock_job(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(remote.analyze_page(0, &source, &raster));
        });
        let answer = rx.recv_timeout(Duration::from_secs(3));
        drop(walker_lock);
        assert!(answer.expect("cloud producer waited on the walker's job lock").is_ok());
        assert_eq!(sent.load(Ordering::SeqCst), 1);
    }

    /// The producer's answer for page 0 of `path` while the walker holds the
    /// job, the way a run's producer asks: bound to page 0 at source index 0
    /// with `bound_sha`, sending `source`. `None` when it waited on the lock.
    fn analysis_under_the_walkers_lock(path: &Path, profile: &str, bound_sha: String, source: Vec<u8>)
        -> (Option<Result<RemotePage, String>>, usize) {
        let raster = decode(&source).unwrap();
        let gateway = gateway();
        let sent = Arc::clone(&gateway.sent);
        let mut remote = RemoteDetection::new(Box::new(gateway), fake::scope(profile, &[0], &[SAM]));
        remote.job_path = Some(path.to_path_buf());
        remote.page_sources = vec![GrantedPage { page_index: 0, source_idx: 0, source_sha256: bound_sha }];
        let walker_lock = crate::run::lock_job(path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(remote.analyze_page(0, &source, &raster));
        });
        let answer = rx.recv_timeout(Duration::from_secs(3)).ok();
        drop(walker_lock);
        (answer, sent.load(Ordering::SeqCst))
    }

    /// The lock-free reading keeps the source binding: a page whose bytes
    /// changed since consent is refused while the walker holds the job, and
    /// refused at once rather than after the walker lets go.
    #[test]
    fn cloud_page_analysis_under_the_walkers_lock_refuses_a_changed_source() {
        let path = chapter("walker-lock-changed-source", 1, StripMode::Single);
        let bound = std::fs::read(Job::open(&path).unwrap().source_path(0).unwrap()).unwrap();
        let (mut raster, _) = page_raster();
        raster.data[0] = 17;
        let changed = cleaner_core::image::encode(&raster, Format::Png).unwrap();
        let (answer, sent) = analysis_under_the_walkers_lock(&path, "modal-prof-walker-changed", hex(&bound), changed);
        assert_eq!(answer.expect("cloud producer waited on the walker's job lock").unwrap_err(),
            "cloud_run_source_changed");
        assert_eq!(sent, 0);
    }

    /// And the page binding: a page the walker's own job moved to another
    /// source since consent is refused, even with identical pixels, without
    /// waiting on the lock the walker still holds.
    #[test]
    fn cloud_page_analysis_under_the_walkers_lock_refuses_a_remapped_page() {
        let path = chapter("walker-lock-remapped", 2, StripMode::Single);
        let source = std::fs::read(Job::open(&path).unwrap().source_path(0).unwrap()).unwrap();
        let mut job = Job::open(&path).unwrap();
        job.project.strip.order.swap(0, 1);
        job.flush().unwrap();
        let (answer, sent) = analysis_under_the_walkers_lock(&path, "modal-prof-walker-remapped", hex(&source), source);
        assert_eq!(answer.expect("cloud producer waited on the walker's job lock").unwrap_err(),
            "cloud_run_source_changed");
        assert_eq!(sent, 0);
    }

    /// A manifest the snapshot cannot vouch for, from a later build, is a
    /// changed source too: the check is refused, never skipped.
    #[test]
    fn cloud_page_analysis_under_the_walkers_lock_refuses_an_unreadable_manifest() {
        let path = chapter("walker-lock-version", 1, StripMode::Single);
        let source = std::fs::read(Job::open(&path).unwrap().source_path(0).unwrap()).unwrap();
        let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["version"] = serde_json::json!(cleaner_core::project::FORMAT_VERSION + 1);
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let (answer, sent) = analysis_under_the_walkers_lock(&path, "modal-prof-walker-version", hex(&source), source);
        assert_eq!(answer.expect("cloud producer waited on the walker's job lock").unwrap_err(),
            "cloud_run_source_changed");
        assert_eq!(sent, 0);
    }

    struct AfterFirstTile {
        gateway: Gateway,
        profile: &'static str,
        selected: Arc<AtomicBool>,
        revoke_epoch: bool,
    }

    impl AnalysisGateway for AfterFirstTile {
        fn capabilities(&self) -> Result<AnalysisCapabilities, String> { self.gateway.capabilities() }
        fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String> {
            let answer = self.gateway.analyze(request, png)?;
            self.selected.store(false, Ordering::SeqCst);
            if self.revoke_epoch {
                GrantService::global().invalidate_profile(CloudProvider::Modal, self.profile);
            }
            Ok(answer)
        }
    }

    #[test]
    fn tile_time_profile_selection_and_epoch_checks_stop_the_second_send() {
        for (profile, revoke_epoch) in [("modal-prof-tile-selection", false),
            ("modal-prof-tile-epoch", true)] {
            let path = chapter(profile, 1, StripMode::Single);
            let gateway = gateway();
            let sent = Arc::clone(&gateway.sent);
            let caps = both();
            let proposed = propose_run(&path, "chapter".into(), vec![0], caps.clone(), CloudProvider::Modal,
                profile.into(), "Modal".into(), "f".repeat(64), &gateway).unwrap();
            let grant = confirm_run(&proposed.proposal_id, true, true).unwrap();
            let mut asked = request(&[0], &caps);
            asked.profile_id = profile;
            asked.profile_epoch = GrantService::global().get_profile_epoch(CloudProvider::Modal, profile);
            let scope = consume_grant(&grant.grant_id, &asked).unwrap();
            let selected = Arc::new(AtomicBool::new(true));
            let client = AfterFirstTile { gateway, profile, selected: Arc::clone(&selected), revoke_epoch };
            let mut remote = RemoteDetection::new(Box::new(client), scope);
            if !revoke_epoch {
                remote.set_allowed(Box::new(move || if selected.load(Ordering::SeqCst) {
                    Ok(())
                } else { Err("cloud_run_profile_changed") }));
            }
            let (raster, source) = page_raster();
            assert_eq!(remote.analyze_page(0, &source, &raster).unwrap_err(), "cloud_run_profile_changed");
            assert_eq!(sent.load(Ordering::SeqCst), 1, "only the first model reached the gateway");
        }
    }

    #[test]
    fn a_changed_model_or_profile_or_cancel_stops_before_any_tile() {
        let path = chapter("stop", 1, StripMode::Single);
        let mut raster = fixtures::by_name("l8").raster;
        raster.width = 64;
        raster.height = 48;
        raster.data = vec![200; 64 * 48];
        let source = cleaner_core::image::encode(&raster, Format::Png).unwrap();
        let caps = vec![SAM.to_string()];
        let open = |gateway: &Gateway| {
            let grant = confirm_run(&proposal(&path, vec![0], vec![SAM], gateway).proposal_id, true, true).unwrap();
            consume_grant(&grant.grant_id, &request(&[0], &caps)).unwrap()
        };

        let changed = gateway();
        let scope = open(&changed);
        let sent = Arc::clone(&changed.sent);
        let remote = RemoteDetection::new(Box::new(Gateway { sent: Arc::clone(&sent), revision: "9".repeat(40), ..gateway() }), scope);
        assert_eq!(remote.analyze_page(0, &source, &raster).unwrap_err(),
            "capability_unavailable: model identity changed");
        assert_eq!(sent.load(Ordering::SeqCst), 0);

        let cancelled = gateway();
        let sent = Arc::clone(&cancelled.sent);
        let scope = open(&cancelled);
        let mut remote = RemoteDetection::new(Box::new(cancelled), scope);
        remote.set_cancel(Arc::new(AtomicBool::new(true)));
        assert_eq!(remote.analyze_page(0, &source, &raster).unwrap_err(), "analysis_cancelled");
        assert_eq!(sent.load(Ordering::SeqCst), 0);

        // Its own profile id: the epoch is process-wide and other tests spend grants.
        let edited = gateway();
        let sent = Arc::clone(&edited.sent);
        let proposed = propose_run(&path, "chapter".into(), vec![0], caps.clone(), CloudProvider::Modal,
            "modal-prof-edit".into(), "Modal".into(), "f".repeat(64), &edited).unwrap();
        let grant = confirm_run(&proposed.proposal_id, true, true).unwrap();
        let mut asked = request(&[0], &caps);
        asked.profile_id = "modal-prof-edit";
        asked.profile_epoch = GrantService::global().get_profile_epoch(CloudProvider::Modal, "modal-prof-edit");
        let scope = consume_grant(&grant.grant_id, &asked).unwrap();
        let remote = RemoteDetection::new(Box::new(edited), scope);
        GrantService::global().invalidate_profile(CloudProvider::Modal, "modal-prof-edit");
        assert_eq!(remote.analyze_page(0, &source, &raster).unwrap_err(), "cloud_run_profile_changed");
        assert_eq!(sent.load(Ordering::SeqCst), 0);
    }

    fn page_raster() -> (Raster, Vec<u8>) {
        let mut raster = fixtures::by_name("l8").raster;
        raster.width = 64;
        raster.height = 48;
        raster.data = vec![200; 64 * 48];
        let source = cleaner_core::image::encode(&raster, Format::Png).unwrap();
        (raster, source)
    }

    /// A chapter run: one consent for its pages, spent once to start, then
    /// held by the run for as long as it takes, whatever the grant's clock.
    #[test]
    fn a_chapter_grant_covers_its_pages_for_the_whole_run_and_no_other_run() {
        let profile = "modal-prof-chapter";
        let path = chapter("chapter-run", 4, StripMode::Single);
        let gateway = gateway();
        let sent = Arc::clone(&gateway.sent);
        let caps = vec![SAM.to_string()];
        let proposed = propose_run(&path, "chapter".into(), vec![0, 1, 2], caps.clone(), CloudProvider::Modal,
            profile.into(), "Modal".into(), "f".repeat(64), &gateway).unwrap();
        assert_eq!((proposed.pages, proposed.total_tiles), (3, 3));
        let grant = confirm_run(&proposed.proposal_id, true, true).unwrap();
        let pages = [0u32, 1, 2];
        let mut asked = request(&pages, &caps);
        asked.profile_id = profile;
        asked.profile_epoch = GrantService::global().get_profile_epoch(CloudProvider::Modal, profile);
        // Spent in the grant's last millisecond: the run it starts is not timed.
        let scope = consume_grant_at(&grant.grant_id, &asked, grant.expires_at_ms - 1).unwrap();
        // A second run cannot present it.
        assert_eq!(consume_grant(&grant.grant_id, &asked).unwrap_err(), "cloud_run_grant_missing");

        let remote = RemoteDetection::new(Box::new(gateway), scope);
        let (raster, source) = page_raster();
        for page in 0..3 {
            remote.analyze_page(page, &source, &raster).unwrap();
        }
        assert_eq!(sent.load(Ordering::SeqCst), 3, "one tile per granted page");
        // Page 3 is in the chapter and outside the consent.
        assert_eq!(remote.analyze_page(3, &source, &raster).unwrap_err(), "cloud_run_page_not_granted");
        assert_eq!(sent.load(Ordering::SeqCst), 3);

        // An edit to the profile mid-run stops the next page, and the run with it.
        GrantService::global().invalidate_profile(CloudProvider::Modal, profile);
        let error = remote.analyze_page(1, &source, &raster).unwrap_err();
        assert_eq!(error, "cloud_run_profile_changed");
        assert!(stops_run(&error));
        assert_eq!(sent.load(Ordering::SeqCst), 3);
    }

    struct Refusing(Gateway);
    impl AnalysisGateway for Refusing {
        fn capabilities(&self) -> Result<AnalysisCapabilities, String> { self.0.capabilities() }
        fn analyze(&self, _: &AnalysisRequest, _: &[u8]) -> Result<AnalysisResult, String> {
            Err(crate::inference::http::HttpTransportError::UnexpectedStatus { status: 401 }.to_string())
        }
    }

    #[test]
    fn a_refused_key_or_cloud_switched_off_ends_the_run_and_other_failures_do_not() {
        let path = chapter("authority", 2, StripMode::Single);
        let caps = vec![SAM.to_string()];
        let open = |gateway: &Gateway| {
            let grant = confirm_run(&proposal(&path, vec![0, 1], vec![SAM], gateway).proposal_id, true, true).unwrap();
            consume_grant(&grant.grant_id, &request(&[0, 1], &caps)).unwrap()
        };
        let (raster, source) = page_raster();

        let scope = open(&gateway());
        let refused = RemoteDetection::new(Box::new(Refusing(gateway())), scope);
        let error = refused.analyze_page(0, &source, &raster).unwrap_err();
        assert!(error.starts_with("gateway_unauthorized: "), "{error}");
        assert!(stops_run(&error));

        let off = gateway();
        let sent = Arc::clone(&off.sent);
        let scope = open(&off);
        let mut remote = RemoteDetection::new(Box::new(off), scope);
        remote.set_allowed(Box::new(|| Err("cloud_disabled")));
        assert_eq!(remote.analyze_page(0, &source, &raster).unwrap_err(), "cloud_disabled");
        assert_eq!(sent.load(Ordering::SeqCst), 0);
        assert!(stops_run("cloud_disabled"));

        for page_only in ["cloud_run_page_not_granted", "analysis_cancelled", "network connection error",
            "Analysis page exceeds the 24 megapixel review limit"] {
            assert!(!stops_run(page_only), "{page_only}");
        }
    }

    /* -------------------------------------------------------------- */
    /* Prefetch                                                        */
    /* -------------------------------------------------------------- */

    /// `count` distinct pages on disk, with the digests the gateway sees them by.
    fn pages_on_disk(name: &str, count: usize) -> (Vec<PathBuf>, Vec<String>) {
        let root = std::env::temp_dir().join(format!("mc-prefetch-{name}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        (0..count).map(|index| {
            let (mut raster, _) = page_raster();
            raster.data = vec![100 + index as u8; 64 * 48];
            let bytes = cleaner_core::image::encode(&raster, Format::Png).unwrap();
            let path = root.join(format!("page{index}.png"));
            std::fs::write(&path, &bytes).unwrap();
            (path, sha256_hex(&bytes))
        }).unzip()
    }

    fn start(gateway: &Gateway, profile: &str, paths: &[PathBuf], order: &[usize], budget: usize)
        -> (Prefetch, Arc<AtomicBool>) {
        let granted: Vec<u32> = order.iter().map(|page| *page as u32).collect();
        let mut remote = RemoteDetection::new(Box::new(gateway.clone()), fake::scope(profile, &granted, &[SAM]));
        let cancel = Arc::new(AtomicBool::new(false));
        remote.set_cancel(Arc::clone(&cancel));
        let pages = order.iter().map(|page| PrefetchPage { page_index: *page, source: Some(paths[*page].clone()), job_path: None })
            .collect();
        (Prefetch::start_with_budget(remote, pages, budget), cancel)
    }

    fn eventually(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn unreadable_detection_source_reports_a_stable_code_without_its_path() {
        let gateway = gateway();
        let remote = RemoteDetection::new(Box::new(gateway),
            fake::scope("modal-prof-unreadable", &[0], &[SAM]));
        let missing = std::env::temp_dir().join(format!("mc-missing-source-{}-secret-path.png",
            std::process::id()));
        let prefetch = Prefetch::start_with_budget(remote,
            vec![PrefetchPage { page_index: 0, source: Some(missing.clone()), job_path: None }], PREFETCH_BUDGET_BYTES);
        let error = prefetch.take(0, "").unwrap_err();
        assert_eq!(error, "cloud_run_source_unreadable");
        assert!(!error.contains(&missing.display().to_string()));
    }

    /// The producer walks the run's pages in the run's order, not sorted, and is
    /// done with all of them before the run has taken one. It releases the GPU
    /// once, after the last page.
    #[test]
    fn prefetch_analyzes_in_run_order_ahead_of_the_run_and_releases_once() {
        let (paths, shas) = pages_on_disk("order", 3);
        let gateway = gateway();
        let (prefetch, _) = start(&gateway, "modal-prof-prefetch-order", &paths, &[1, 0, 2], PREFETCH_BUDGET_BYTES);
        eventually("every page analyzed", || gateway.sent.load(Ordering::SeqCst) == 3);
        assert_eq!(*gateway.seen.lock().unwrap(), vec![shas[1].clone(), shas[0].clone(), shas[2].clone()]);
        eventually("the release", || gateway.releases.load(Ordering::SeqCst) == 1);

        let page = prefetch.take(1, &shas[1]).unwrap();
        assert_eq!((page.width, page.height), (64, 48));
        assert!(page.mask.unwrap().iter().all(|value| *value == 255));
        assert!(prefetch.take(0, &shas[0]).is_ok());
        // A page whose file changed after it was analyzed is not cleaned with
        // another page's answer.
        assert!(prefetch.take(2, &"0".repeat(64)).unwrap_err().starts_with("cloud_run_source_changed"));
        assert_eq!(prefetch.shared.lock().bytes, 0);
        drop(prefetch);
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 1);
        assert_eq!(gateway.sent.load(Ordering::SeqCst), 3, "nothing is sent twice");
    }

    /// A long strip's window reads the next page's first rows, so the run
    /// looks at that page's answer before its turn. Looking takes nothing: the
    /// page is still there for its turn, nothing is sent twice, and a page the
    /// producer was never handed is not waited for.
    #[test]
    fn peeking_at_a_later_page_leaves_it_for_its_turn() {
        let (paths, shas) = pages_on_disk("peek", 3);
        let gateway = gateway();
        let (prefetch, _) = start(&gateway, "modal-prof-prefetch-peek", &paths, &[0, 1], PREFETCH_BUDGET_BYTES);
        assert!(prefetch.take(0, &shas[0]).is_ok());
        let peeked = prefetch.peek(1).expect("page 1 is on its way");
        assert_eq!((peeked.width, peeked.height), (64, 48));
        assert!(prefetch.peek(2).is_none(), "page 2 is not in this run");
        assert!(prefetch.take(1, &shas[1]).is_ok());
        assert!(prefetch.peek(1).is_none(), "a page taken is gone");
        drop(prefetch);
        assert_eq!(gateway.sent.load(Ordering::SeqCst), 2, "nothing is sent twice");
    }

    /// A page that fails in the cloud is held for its turn: the run sees the
    /// failure when it reaches that page, and the pages after it still come.
    /// A page the run skipped without asking is dropped on the way.
    #[test]
    fn a_failed_page_waits_for_its_turn_and_the_rest_go_on() {
        let (paths, shas) = pages_on_disk("failed", 4);
        let gateway = gateway();
        gateway.fail.lock().unwrap().insert(shas[1].clone(), "network connection error".into());
        let (prefetch, _) = start(&gateway, "modal-prof-prefetch-failed", &paths, &[0, 1, 2, 3], PREFETCH_BUDGET_BYTES);
        assert!(prefetch.take(0, &shas[0]).is_ok());
        let error = prefetch.take(1, &shas[1]).unwrap_err();
        assert_eq!(error, "network connection error");
        assert!(!stops_run(&error));
        // Page 2 was never asked for: the run could not read it.
        assert!(prefetch.take(3, &shas[3]).is_ok());
        assert_eq!(gateway.seen.lock().unwrap().len(), 4);
        drop(prefetch);
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 1);
    }

    /// A failure that withdraws the run's authority stops the producer at that
    /// page: nothing after it is sent, and a later page has no answer.
    #[test]
    fn a_failure_that_stops_the_run_stops_the_producer() {
        let (paths, shas) = pages_on_disk("stops", 4);
        let gateway = gateway();
        let refused = crate::inference::http::HttpTransportError::UnexpectedStatus { status: 401 }.to_string();
        gateway.fail.lock().unwrap().insert(shas[1].clone(), refused);
        let (prefetch, _) = start(&gateway, "modal-prof-prefetch-stops", &paths, &[0, 1, 2, 3], PREFETCH_BUDGET_BYTES);
        assert!(prefetch.take(0, &shas[0]).is_ok());
        let error = prefetch.take(1, &shas[1]).unwrap_err();
        assert!(error.starts_with("gateway_unauthorized: ") && stops_run(&error), "{error}");
        assert_eq!(prefetch.take(2, &shas[2]).unwrap_err(), "cloud_run_page_missing");
        assert_eq!(*gateway.seen.lock().unwrap(), vec![shas[0].clone(), shas[1].clone()]);
        drop(prefetch);
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 1);
    }

    /// The run's cancel stops the producer before its next page. The run's end
    /// joins it, and it still releases the GPU it used.
    #[test]
    fn cancel_stops_the_producer_before_its_next_page() {
        let (paths, shas) = pages_on_disk("cancel", 4);
        let mut gateway = gateway();
        gateway.hold = Some(Arc::default());
        let (prefetch, cancel) = start(&gateway, "modal-prof-prefetch-cancel", &paths, &[0, 1, 2, 3], PREFETCH_BUDGET_BYTES);
        gateway.release_page(&shas[0]);
        assert!(prefetch.take(0, &shas[0]).is_ok());
        cancel.store(true, Ordering::SeqCst);
        // Page 1 may already be waiting at the gateway; it finishes, nothing after it starts.
        gateway.release_page(&shas[1]);
        gateway.release_page(&shas[2]);
        gateway.release_page(&shas[3]);
        assert_eq!(prefetch.take(2, &shas[2]).unwrap_err(), "analysis_cancelled");
        drop(prefetch);
        let seen = gateway.seen.lock().unwrap().clone();
        assert!(!seen.contains(&shas[2]) && !seen.contains(&shas[3]), "sent after the cancel");
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 1);
    }

    /// The producer waits when the buffer is full, and never holds more than the
    /// budget in it. A run that ends while it waits does not leave it waiting.
    #[test]
    fn the_buffer_holds_no_more_than_its_budget() {
        let (paths, shas) = pages_on_disk("budget", 5);
        let gateway = gateway();
        let page_bytes = 64 * 48;
        let budget = 2 * page_bytes;
        let (prefetch, _) = start(&gateway, "modal-prof-prefetch-budget", &paths, &[0, 1, 2, 3, 4], budget);
        // Two pages buffered and a third analyzed and waiting for room.
        eventually("a full buffer", || gateway.sent.load(Ordering::SeqCst) == 3);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(gateway.sent.load(Ordering::SeqCst), 3, "the producer ran past its budget");
        assert_eq!(prefetch.shared.lock().bytes, budget);
        assert!(prefetch.take(0, &shas[0]).is_ok());
        eventually("room used again", || gateway.sent.load(Ordering::SeqCst) == 4);
        assert!(prefetch.shared.lock().bytes <= budget);
        // The run ends here, with the producer waiting for room again.
        drop(prefetch);
        assert_eq!(gateway.sent.load(Ordering::SeqCst), 4);
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 1);

        // A single page larger than the budget still goes through, alone.
        let gateway = fake::gateway();
        let (prefetch, _) = start(&gateway, "modal-prof-prefetch-small", &paths, &[0, 1], 1);
        assert!(prefetch.take(0, &shas[0]).is_ok());
        assert!(prefetch.take(1, &shas[1]).is_ok());
    }

    /// Nothing sent, nothing released; and no release once cloud engines are off
    /// or the profile changed, since the run no longer speaks for the endpoint.
    #[test]
    fn the_release_is_only_asked_for_by_a_run_that_sent_and_still_may() {
        let gateway = gateway();
        let (prefetch, cancel) = start(&gateway, "modal-prof-prefetch-none", &[], &[], PREFETCH_BUDGET_BYTES);
        drop(prefetch);
        drop(cancel);
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 0);

        let mut remote = RemoteDetection::new(Box::new(gateway.clone()),
            fake::scope("modal-prof-prefetch-off", &[0], &[SAM]));
        remote.set_allowed(Box::new(|| Err("cloud_disabled")));
        remote.release_idle();
        let remote = RemoteDetection::new(Box::new(gateway.clone()),
            fake::scope("modal-prof-prefetch-edited", &[0], &[SAM]));
        GrantService::global().invalidate_profile(CloudProvider::Modal, "modal-prof-prefetch-edited");
        remote.release_idle();
        assert_eq!(gateway.releases.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_chapter_past_the_page_cap_is_refused_before_anything_is_read() {
        assert_eq!(normalized_pages((0..=MAX_RUN_PAGES as u32).collect()).unwrap_err(), "cloud_run_too_many_pages");
        assert_eq!(normalized_pages((0..MAX_RUN_PAGES as u32).collect()).unwrap().len(), MAX_RUN_PAGES);
        assert!(MAX_RUN_TILE_PIXELS >= MAX_RUN_PAGES as u64 * 3_840_000, "a chapter of ordinary scans fits");
    }
}
