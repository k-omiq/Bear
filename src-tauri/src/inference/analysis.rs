//! Explicit, review-only cloud page analysis. Each proposal covers one page and
//! one sequential batch. A failed or interrupted batch never resubmits itself.
//!
//! A proposal may name a second model for the same tiles, so a preset that
//! needs both RT-DETR boxes and the SAM mask asks for consent once. The tile
//! loop ([`run_tiles`]) is shared with Auto clean's cloud detection
//! (`inference::run_analysis`), which uploads the raw source page under a
//! run-level grant instead of a per-page review proposal.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cleaner_core::balloon::{BalloonBox, BalloonClass};
use cleaner_core::cloud_analysis_wire::{
    self, AnalysisBatch, AnalysisBatchResult, AnalysisCapabilities, AnalysisRequest, AnalysisResult, TileRect, RT,
    SAM, VERSION,
};
use cleaner_core::cloud_job_wire::JobKind;
use cleaner_core::cloud_decode::decode_analysis_mask;
use cleaner_core::cloud_tiles;
use cleaner_core::engines::render::{CloudProvider, RenderRecipe};
use cleaner_core::image::Raster;
use cleaner_core::mask::Rect;
use cleaner_core::project::{Job, StripMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::inference::http::{CloudHttpClient, HttpTransportError};
use crate::inference::journal::{
    AnalysisAttemptRecord, AnalysisJournal, AnalysisPhase, ANALYSIS_JOURNAL_SCHEMA_VERSION,
};
use crate::inference::policy::{GrantScope, GrantService};
use crate::library::Library;
use crate::run;

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum AnalysisFlowError {
    #[error("rights_attestation_required")]
    RightsAttestationRequired,
    #[error("retention_acknowledgement_required")]
    RetentionAcknowledgementRequired,
    #[error("analysis_stale: {0}")]
    Stale(&'static str),
    #[error("analysis_cancelled")]
    Cancelled,
    #[error("capability_unavailable: {0}")]
    CapabilityUnavailable(&'static str),
    #[error("{0}")]
    Other(String),
}

impl From<String> for AnalysisFlowError {
    fn from(value: String) -> Self { Self::Other(value) }
}

impl From<&'static str> for AnalysisFlowError {
    fn from(value: &'static str) -> Self { Self::Other(value.into()) }
}

pub(crate) fn require_consent(rights_attested: bool, retention_acknowledged: bool)
    -> Result<(), AnalysisFlowError> {
    if !rights_attested { return Err(AnalysisFlowError::RightsAttestationRequired); }
    if !retention_acknowledged { return Err(AnalysisFlowError::RetentionAcknowledgementRequired); }
    Ok(())
}

fn require_active(cancel: &AtomicBool) -> Result<(), AnalysisFlowError> {
    if cancel.load(Ordering::SeqCst) { return Err(AnalysisFlowError::Cancelled); }
    Ok(())
}

fn require_unchanged(unchanged: bool, context: &'static str) -> Result<(), AnalysisFlowError> {
    if !unchanged { return Err(AnalysisFlowError::Stale(context)); }
    Ok(())
}

pub(crate) trait AnalysisGateway {
    fn capabilities(&self) -> Result<AnalysisCapabilities, String>;
    fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String>;

    /// [`Self::analyze`], given up once `cancel` is set while the tile is out.
    /// The HTTP client submits and polls the tile (`inference::gpu_jobs`), so it
    /// stops polling and cancels the tile on the gateway; a gateway that cannot
    /// (the default, an older synchronous gateway) waits for the answer.
    fn analyze_cancellable(&self, request: &AnalysisRequest, png: &[u8], _cancel: &AtomicBool)
        -> Result<AnalysisResult, String> {
        self.analyze(request, png)
    }

    /// Release the analysis GPU now if the gateway sees it idle with no tile in
    /// flight (`POST /mc/v1/gpu/stop` with `idle_only`). A gateway that cannot says
    /// so with an `Err`, and the caller only logs it: the GPU then scales down on its
    /// own idle timer.
    fn release_idle(&self) -> Result<(), String> {
        Err("gpu_release_unsupported".into())
    }

    /// Whether [`Self::analyze_batch`] is served. Not by default (an older
    /// gateway, Beam): [`run_tiles`] then sends the requests one by one.
    fn serves_batches(&self) -> Result<bool, String> {
        Ok(false)
    }

    /// One batch of a page's requests in one call, its distinct tile PNGs in
    /// `tiles` ([`AnalysisBatch`]), given up once `cancel` is set while it is out.
    fn analyze_batch(&self, _batch: &AnalysisBatch, _tiles: &[&[u8]], _cancel: &AtomicBool)
        -> Result<AnalysisBatchResult, String> {
        Err("analysis_batch_unsupported".into())
    }
}

impl AnalysisGateway for CloudHttpClient {
    fn capabilities(&self) -> Result<AnalysisCapabilities, String> {
        self.get_analysis_capabilities().map_err(|e| match e {
            HttpTransportError::WireValidation | HttpTransportError::JsonDeserialization =>
                "capability_unavailable: gateway advertisement is invalid".into(),
            HttpTransportError::UnexpectedStatus { status: 503 } =>
                "capability_unavailable: gateway analysis is not configured".into(),
            other => other.to_string(),
        })
    }

    fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String> {
        self.analyze_cancellable(request, png, &AtomicBool::new(false))
    }

    fn analyze_cancellable(&self, request: &AnalysisRequest, png: &[u8], cancel: &AtomicBool)
        -> Result<AnalysisResult, String> {
        self.analyze_tile(request, png, cancel).map_err(|e| e.to_string())
    }

    fn release_idle(&self) -> Result<(), String> {
        crate::inference::gpu::release_idle(self, crate::inference::gpu::GpuRole::Analysis).map(|_| ())
    }

    fn serves_batches(&self) -> Result<bool, String> {
        self.serves_gpu_jobs(JobKind::AnalysisBatch).map_err(|e| e.to_string())
    }

    fn analyze_batch(&self, batch: &AnalysisBatch, tiles: &[&[u8]], cancel: &AtomicBool)
        -> Result<AnalysisBatchResult, String> {
        self.submit_analysis_batch(batch, tiles, cancel).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TileDisclosure {
    pub rect: TileRect,
    pub png_sha256: String,
    pub encoded_bytes: usize,
    pub input_sha256: String,
    pub predecessors_sha256: String,
}

/// One advertised analysis model, pinned by the identity the user consented to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModel {
    pub capability: String,
    pub graph_sha256s: Vec<String>,
    pub model_revision: String,
}

impl RemoteModel {
    fn of(advertised: &cleaner_core::cloud_analysis_wire::AnalysisCapability) -> Self {
        Self {
            capability: advertised.capability.clone(),
            graph_sha256s: advertised.graph_sha256s.clone(),
            model_revision: advertised.model_revision.clone(),
        }
    }
}

/// The gateway's current identity for each consented model, or the typed
/// refusal the review and the run both show. Nothing is sent before this.
pub(crate) fn current_models(available: &AnalysisCapabilities, consented: &[RemoteModel])
    -> Result<(), String> {
    for model in consented {
        let current = capability(available, &model.capability).map_err(|e| e.to_string())?;
        if current.graph_sha256s != model.graph_sha256s || current.model_revision != model.model_revision {
            return Err(AnalysisFlowError::CapabilityUnavailable("model identity changed").to_string());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisProposal {
    pub proposal_id: String,
    pub chapter_id: String,
    pub page_index: usize,
    pub provider: CloudProvider,
    pub profile_id: String,
    pub profile_name: String,
    pub capability: String,
    pub graph_sha256s: Vec<String>,
    pub model_revision: String,
    pub source_page_sha256: String,
    pub underlay_sha256: String,
    pub predecessors_sha256: String,
    pub project_revision_sha256: String,
    pub page_width: u32,
    pub page_height: u32,
    pub pages: u32,
    pub total_tile_pixels: u64,
    pub total_encoded_bytes: u64,
    pub includes_surrounding_art: bool,
    pub cost_estimate_usd: Option<f64>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub tiles: Vec<TileDisclosure>,
    /// Every model the tiles go to, `capability` first. One entry unless the
    /// review asked for RT-DETR boxes and the SAM mask together.
    #[serde(default)]
    pub models: Vec<RemoteModel>,
}

struct StoredProposal {
    proposal: AnalysisProposal,
    job_path: PathBuf,
    endpoint_fingerprint: String,
    profile_epoch: u64,
    confirmed: bool,
    finished: bool,
    cancel: Arc<AtomicBool>,
    #[cfg(test)]
    before_submit: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn proposals() -> &'static Mutex<HashMap<String, StoredProposal>> {
    static PROPOSALS: OnceLock<Mutex<HashMap<String, StoredProposal>>> = OnceLock::new();
    PROPOSALS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(debug_assertions)]
pub(crate) fn proposal_endpoint_fingerprint(proposal_id: &str) -> Result<String, String> {
    proposals().lock().map_err(|_| "analysis proposal unavailable")?
        .get(proposal_id).map(|stored| stored.endpoint_fingerprint.clone())
        .ok_or_else(|| "analysis proposal unavailable".into())
}

fn hex(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

pub(crate) fn random_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "Random source unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn source_page(job: &Job, page_index: usize) -> Result<(usize, Vec<u8>, Raster), String> {
    if job.project.strip.mode != StripMode::Single {
        return Err("Remote analysis currently requires a paginated chapter".into());
    }
    let source_idx = Library::resolve_page(&job.project, page_index)
        .ok_or("Page is no longer in chapter")?;
    let declared = job.project.sources.get(source_idx).ok_or("Chapter source is missing")?;
    if u64::from(declared.w) * u64::from(declared.h) > 24_000_000 {
        return Err("Analysis page exceeds the 24 megapixel review limit".into());
    }
    let path = job.source_path(source_idx).ok_or("Chapter source is missing")?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() > 20_000_000 { return Err("Source page exceeds the review-preview limit".into()); }
    crate::model_workflows::source_mime(&bytes)?;
    let page = cleaner_core::image::decode(&bytes)
        .or_else(|_| cleaner_core::image::foreign::decode(&bytes))
        .map_err(|e| e.to_string())?;
    if page.width != declared.w || page.height != declared.h
        || u64::from(page.width) * u64::from(page.height) > 24_000_000 {
        return Err("Analysis source geometry changed or exceeds the review limit".into());
    }
    let page = cleaner_core::image::orientation::from_bytes(&bytes).raster(&page);
    Ok((source_idx, bytes, page))
}

type PreparedTiles = (Vec<u8>, u32, u32, Vec<(TileDisclosure, Vec<u8>)>);

/// Which tiles [`prepared_tiles`] encodes.
#[derive(Clone, Copy)]
enum TileChoice<'a> {
    /// A proposal: every tile of the plan whose core meets one of these
    /// regions (the whole page when empty).
    Regions(&'a [TileRect]),
    /// A confirmation: exactly the approved tiles, never replanned. A tile
    /// rectangle reaches into its neighbours' cores, so planning from the
    /// approved rectangles would add tiles nobody approved.
    Approved(&'a [TileRect]),
}

fn prepared_tiles(
    job_path: &Path,
    page_index: usize,
    choice: TileChoice<'_>,
) -> Result<PreparedTiles, String> {
    let _lock = run::lock_job(job_path)?;
    let job = Job::open(job_path).map_err(|e| e.to_string())?;
    let (source_idx, source, page) = source_page(&job, page_index)?;
    let rects = match choice {
        TileChoice::Regions(regions) => cloud_tiles::plan(page.width, page.height, regions)?.tiles,
        TileChoice::Approved(approved) => {
            if approved.iter().any(|&rect| cloud_tiles::core_of(page.width, page.height, rect).is_none()) {
                return Err("analysis tile is not on the page's tile plan".into());
            }
            approved.to_vec()
        }
    };
    let mut tiles = Vec::with_capacity(rects.len());
    for rect in rects {
        let input = crate::underlay::read_exact(&job, source_idx, &page,
            Rect::new(rect.x as i64, rect.y as i64, rect.width, rect.height),
            u32::MAX, None)?;
        let (_, png) = encode_tile(&input.image, TileRect {
            x: 0, y: 0, width: rect.width, height: rect.height,
        })?;
        tiles.push((TileDisclosure {
            rect, png_sha256: hex(&png), encoded_bytes: png.len(),
            input_sha256: input.digest, predecessors_sha256: input.predecessors,
        }, png));
    }
    Ok((source, page.width, page.height, tiles))
}

fn joined_digest<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let mut hash = Sha256::new();
    for value in values { hash.update(value.as_bytes()); hash.update(b"\0"); }
    format!("{:x}", hash.finalize())
}

fn project_revision(job_path: &Path, page_index: usize) -> Result<String, String> {
    let _lock = run::lock_job(job_path)?;
    project_revision_locked(job_path, page_index)
}

fn project_revision_locked(job_path: &Path, page_index: usize) -> Result<String, String> {
    let job = Job::open(job_path).map_err(|e| e.to_string())?;
    let source_idx = Library::resolve_page(&job.project, page_index)
        .ok_or("Page is no longer in chapter")?;
    let source = job.source_path(source_idx).ok_or("Chapter source is missing")?;
    let mut hash = Sha256::new();
    hash.update(b"analysis_project_revision_v1\0");
    hash.update(std::fs::read(source).map_err(|e| e.to_string())?);
    for record in &job.project.patches {
        if record.source_idx != source_idx { continue; }
        hash.update(serde_json::to_vec(record).map_err(|e| e.to_string())?);
        let patch = job.load_patch(record).map_err(|e| e.to_string())?;
        hash.update(&patch.mask.bits);
        hash.update(&patch.ink.bits);
        hash.update(&patch.pixels.data);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn capability<'a>(available: &'a AnalysisCapabilities, id: &str)
    -> Result<&'a cleaner_core::cloud_analysis_wire::AnalysisCapability, AnalysisFlowError> {
    available.validate().map_err(|_| AnalysisFlowError::CapabilityUnavailable("gateway advertisement is invalid"))?;
    available.capabilities.iter().find(|entry| entry.capability == id)
        .ok_or(AnalysisFlowError::CapabilityUnavailable("gateway does not advertise this analysis model"))
}

fn check_gateway_limits(available: &AnalysisCapabilities, tiles: &[TileDisclosure]) -> Result<(), AnalysisFlowError> {
    if tiles.iter().any(|tile| exceeds_limits(available, tile.rect, Some(tile.encoded_bytes))) {
        return Err(AnalysisFlowError::CapabilityUnavailable("gateway tile limits are smaller than this upload"));
    }
    Ok(())
}

/// Whether one tile is larger than the gateway accepts. `encoded` is `None`
/// while only the geometry is known, as when a run is proposed.
pub(crate) fn exceeds_limits(available: &AnalysisCapabilities, rect: TileRect, encoded: Option<usize>) -> bool {
    rect.width > available.limits.max_tile_side
        || rect.height > available.limits.max_tile_side
        || u64::from(rect.width) * u64::from(rect.height) > available.limits.max_tile_pixels
        || encoded.is_some_and(|bytes| bytes > available.limits.max_png_bytes)
}

pub(crate) fn gateway_limit_error() -> String {
    AnalysisFlowError::CapabilityUnavailable("gateway tile limits are smaller than this upload").to_string()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn propose<G: AnalysisGateway>(
    job_path: PathBuf,
    chapter_id: String,
    page_index: usize,
    provider: CloudProvider,
    profile_id: String,
    profile_name: String,
    endpoint_fingerprint: String,
    capability_id: String,
    companion_id: Option<String>,
    regions: Vec<TileRect>,
    client: &G,
    journal: &AnalysisJournal,
) -> Result<AnalysisProposal, String> {
    if !matches!(capability_id.as_str(), SAM | RT)
        || companion_id.as_deref().is_some_and(|id| !matches!(id, SAM | RT) || id == capability_id) {
        return Err("capability_unavailable: unsupported analysis capability".into());
    }
    let available = client.capabilities()?;
    let advertised = capability(&available, &capability_id).map_err(|e| e.to_string())?;
    let mut models = vec![RemoteModel::of(advertised)];
    if let Some(id) = &companion_id {
        models.push(RemoteModel::of(capability(&available, id).map_err(|e| e.to_string())?));
    }
    let (source, page_width, page_height, tiles) = prepared_tiles(&job_path, page_index, TileChoice::Regions(&regions))?;
    let disclosures: Vec<_> = tiles.into_iter().map(|(tile, _)| tile).collect();
    check_gateway_limits(&available, &disclosures).map_err(|e| e.to_string())?;
    let total_tile_pixels = disclosures.iter().map(|t| u64::from(t.rect.width) * u64::from(t.rect.height)).sum();
    let total_encoded_bytes = disclosures.iter().map(|t| t.encoded_bytes as u64).sum();
    let underlay_sha256 = joined_digest(disclosures.iter().map(|t| t.input_sha256.as_str()));
    let predecessors_sha256 = joined_digest(disclosures.iter().map(|t| t.predecessors_sha256.as_str()));
    let issued_at_ms = now_ms();
    let proposal = AnalysisProposal {
        proposal_id: random_id()?, chapter_id, page_index, provider,
        profile_id, profile_name, capability: capability_id,
        graph_sha256s: advertised.graph_sha256s.clone(), model_revision: advertised.model_revision.clone(),
        source_page_sha256: hex(&source), underlay_sha256, predecessors_sha256,
        project_revision_sha256: project_revision(&job_path, page_index)?,
        page_width, page_height, pages: 1, total_tile_pixels, total_encoded_bytes,
        includes_surrounding_art: true, cost_estimate_usd: None, tiles: disclosures,
        issued_at_ms, expires_at_ms: issued_at_ms.saturating_add(
            crate::inference::consent::DEFAULT_PROPOSAL_TTL.as_millis() as u64),
        models,
    };
    let record = AnalysisAttemptRecord {
        created_at_ms: issued_at_ms,
        schema_version: ANALYSIS_JOURNAL_SCHEMA_VERSION,
        proposal_id: proposal.proposal_id.clone(), provider, profile_id: proposal.profile_id.clone(),
        capability: proposal.capability.clone(), source_sha256: proposal.source_page_sha256.clone(),
        underlay_sha256: proposal.underlay_sha256.clone(),
        model_identity_sha256: None, chapter_id: Some(proposal.chapter_id.clone()),
        page_index: Some(proposal.page_index as u32),
        total_tiles: proposal.tiles.len() * proposal.models.len(),
        completed_tiles: 0, reported_cost_usd: None,
        tile_submission_started: Some(false), cancel_requested: false,
        phase: AnalysisPhase::Proposed,
    };
    let epoch = GrantService::global().get_profile_epoch(provider, &proposal.profile_id);
    let mut cache = proposals().lock().map_err(|e| e.to_string())?;
    cache.retain(|_, stored| stored.confirmed || stored.proposal.expires_at_ms > issued_at_ms);
    if cache.len() >= crate::inference::consent::MAX_CACHED_PROPOSALS {
        let oldest = cache.iter().filter(|(_, stored)| stored.finished)
            .min_by_key(|(_, stored)| stored.proposal.issued_at_ms)
            .map(|(id, _)| id.clone());
        if let Some(id) = oldest { cache.remove(&id); }
        else { return Err("analysis_proposal_limit".into()); }
    }
    journal.write(&record).map_err(|e| e.to_string())?;
    cache.insert(proposal.proposal_id.clone(), StoredProposal {
        proposal: proposal.clone(), job_path, endpoint_fingerprint, profile_epoch: epoch,
        confirmed: false, finished: false, cancel: Arc::new(AtomicBool::new(false)),
        #[cfg(test)]
        before_submit: None,
    });
    Ok(proposal)
}

fn scope(proposal: &AnalysisProposal, endpoint_fingerprint: &str, model: &RemoteModel) -> GrantScope {
    GrantScope {
        capability: model.capability.clone(), provider: proposal.provider,
        profile_id: proposal.profile_id.clone(),
        canonical_endpoint_fingerprint: endpoint_fingerprint.to_string(),
        source_hash: proposal.source_page_sha256.clone(),
        crop_sha256: proposal.tiles[0].png_sha256.clone(),
        hint_sha256: proposal.underlay_sha256.clone(),
        operation_digest: hex(proposal.proposal_id.as_bytes()),
        crop_bounds: Rect::new(0, 0, proposal.page_width, proposal.page_height),
        mask_hash: proposal.underlay_sha256.clone(), revision: 0,
        input_sha256: proposal.underlay_sha256.clone(),
        predecessors_sha256: proposal.predecessors_sha256.clone(),
        recipe: RenderRecipe::new(&model.capability, VERSION, &model.capability,
            &model.model_revision, false),
        region_ids: vec![format!("page_{}", proposal.page_index)],
    }
}

fn same_tiles(proposal: &AnalysisProposal, source: &[u8], width: u32, height: u32,
    tiles: &[(TileDisclosure, Vec<u8>)]) -> bool {
    width == proposal.page_width && height == proposal.page_height
        && hex(source) == proposal.source_page_sha256
        && tiles.len() == proposal.tiles.len()
        && tiles.iter().zip(&proposal.tiles).all(|((actual, _), approved)|
            actual.rect == approved.rect && actual.png_sha256 == approved.png_sha256
            && actual.input_sha256 == approved.input_sha256
            && actual.predecessors_sha256 == approved.predecessors_sha256)
}

#[allow(clippy::too_many_arguments)]
fn tile_request(model: &RemoteModel, source_page_sha256: &str, index: usize, rect: TileRect,
    png_sha256: &str, page_width: u32, page_height: u32) -> Result<AnalysisRequest, String> {
    let tile_core = cloud_tiles::core_of(page_width, page_height, rect)
        .ok_or("analysis tile is not on the page's tile plan")?;
    let mut request = AnalysisRequest {
        protocol_version: VERSION.into(), capability: model.capability.clone(),
        graph_sha256s: model.graph_sha256s.clone(), model_revision: model.model_revision.clone(),
        tile_id: format!("tile_{index}"), tile_rect: rect,
        tile_png_sha256: png_sha256.to_string(),
        source_page_sha256: source_page_sha256.to_string(),
        page_width, page_height, tile_core, request_digest: String::new(),
    };
    request.request_digest = request.digest()?;
    Ok(request)
}

/// One tile ready to send: where it sits, its PNG digest, and the PNG.
pub(crate) type TileUpload<'a> = (TileRect, &'a str, &'a [u8]);

/// What the tiles of one page came back as, stitched into page coordinates:
/// each mask pixel from the tile whose core owns it, and every tile's boxes
/// joined once ([`cloud_tiles::merge_tile_boxes`]).
pub(crate) struct Stitched {
    pub mask: Option<Vec<u8>>,
    pub boxes: Vec<BalloonBox>,
}

/// The tile loop both the review and the run use. Every tile goes to every
/// model in order. A gateway that serves batches gets them a batch at a time
/// ([`cloud_analysis_wire::batches`], each tile PNG sent once), else one
/// request at a time. `before` runs once per request before it is sent and may
/// refuse it, and `after` sees each validated answer, both in request order. A
/// failure stops the loop: nothing is retried and nothing else is sent. A
/// `cancel` set while a request is out ends it as
/// [`AnalysisFlowError::Cancelled`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_tiles<G: AnalysisGateway + ?Sized>(
    client: &G,
    models: &[RemoteModel],
    source_page_sha256: &str,
    tiles: &[TileUpload<'_>],
    width: u32,
    height: u32,
    cancel: &AtomicBool,
    before: &mut dyn FnMut(usize) -> Result<(), AnalysisFlowError>,
    after: &mut dyn FnMut(usize, &AnalysisResult) -> Result<(), AnalysisFlowError>,
) -> Result<Stitched, AnalysisFlowError> {
    let page_pixels = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| "analysis page too large")?;
    let mut mask = models.iter().any(|model| model.capability == SAM).then(|| vec![0u8; page_pixels]);
    let mut boxes = Vec::new();
    let mut steps: Vec<(AnalysisRequest, &[u8])> = Vec::with_capacity(tiles.len() * models.len());
    for (tile_index, (rect, png_sha256, png)) in tiles.iter().enumerate() {
        for model in models {
            let request = tile_request(model, source_page_sha256, tile_index, *rect, png_sha256, width, height)?;
            request.validate(png)?;
            steps.push((request, *png));
        }
    }
    // A cancel while a request is out gives it up (and cancels it on a gateway
    // that polls); the journal then treats it as the caller's cancel always has.
    let sent = |error: String| {
        if cancel.load(Ordering::SeqCst) { AnalysisFlowError::Cancelled } else { AnalysisFlowError::Other(error) }
    };
    if client.serves_batches().map_err(AnalysisFlowError::Other)? {
        let planned: Vec<(&AnalysisRequest, &[u8])> = steps.iter().map(|(request, png)| (request, *png)).collect();
        for range in cloud_analysis_wire::batches(&planned) {
            for step in range.clone() { before(step)?; }
            let batch = AnalysisBatch::new(steps[range.clone()].iter().map(|(request, _)| request.clone()).collect());
            let mut pngs: Vec<&[u8]> = Vec::new();
            for (index, (request, png)) in steps[range.clone()].iter().enumerate() {
                if index == 0 || steps[range.start + index - 1].0.tile_png_sha256 != request.tile_png_sha256 {
                    pngs.push(png);
                }
            }
            let answer = client.analyze_batch(&batch, &pngs, cancel).map_err(sent)?;
            answer.validate(&batch)?;
            for ((step, request), result) in range.zip(&batch.requests).zip(&answer.results) {
                stitch(result, request.tile_core, &mut mask, &mut boxes, width, height)?;
                after(step, result)?;
            }
        }
    } else {
        for (step, (request, png)) in steps.iter().enumerate() {
            before(step)?;
            let result = client.analyze_cancellable(request, png, cancel).map_err(sent)?;
            result.validate(request)?;
            stitch(&result, request.tile_core, &mut mask, &mut boxes, width, height)?;
            after(step, &result)?;
        }
    }
    let boxes = cloud_tiles::merge_tile_boxes(width, height, &boxes);
    Ok(Stitched { mask, boxes })
}

/// A tile of a decoded page as the PNG the wire carries.
pub(crate) fn encode_tile(page: &Raster, rect: TileRect) -> Result<(String, Vec<u8>), String> {
    use cleaner_core::image::color::{ManagedColor, alpha, byte, encoded, linear};
    let managed = ManagedColor::for_raster(page).map_err(|error| error.to_string())?;
    let mut rgb = Vec::with_capacity((rect.width * rect.height * 3) as usize);
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            let coverage = alpha(page, x, y);
            let pixel = managed.pixel(page, x, y)
                .map(|value| byte(encoded(linear(value) * coverage + 1.0 - coverage)));
            rgb.extend_from_slice(&pixel);
        }
    }
    // Transport pixels are an explicit sRGB view; the original profile and native
    // samples continue to identify the page and never attach to these pixels.
    let transport = cleaner_core::image::Raster {
        width: rect.width, height: rect.height,
        mode: cleaner_core::image::ColorMode::Rgb,
        depth: cleaner_core::image::BitDepth::Eight,
        icc: None, palette: None, trns: None, srgb_intent: Some(1),
        color: Default::default(), data: rgb,
    };
    let png = cleaner_core::image::encode(&transport, cleaner_core::image::Format::Png)
        .map_err(|error| error.to_string())?;
    if png.len() > cleaner_core::cloud_analysis_wire::MAX_PNG_BYTES {
        return Err("Analysis tile PNG exceeds transfer limit".into());
    }
    Ok((hex(&png), png))
}

/// The capability label stored with fused evidence: one id, or both joined.
fn capability_label(models: &[RemoteModel]) -> String {
    models.iter().map(|model| model.capability.as_str()).collect::<Vec<_>>().join("+")
}

pub fn cancel(proposal_id: &str) -> Result<bool, String> {
    let proposals = proposals().lock().map_err(|e| e.to_string())?;
    let Some(stored) = proposals.get(proposal_id) else { return Ok(false) };
    if stored.finished { return Ok(false); }
    stored.cancel.store(true, Ordering::SeqCst);
    Ok(true)
}

pub(crate) fn confirm_and_run<G: AnalysisGateway>(
    proposal_id: &str,
    rights_attested: bool,
    retention_acknowledged: bool,
    client: &G,
    journal: &AnalysisJournal,
    progress: impl Fn(&AnalysisAttemptRecord),
) -> Result<crate::model_workflows::Analysis, String> {
    require_consent(rights_attested, retention_acknowledged).map_err(|e| e.to_string())?;
    let (proposal, path, endpoint_fingerprint, epoch, cancel) = {
        let mut map = proposals().lock().map_err(|e| e.to_string())?;
        let stored = map.get_mut(proposal_id).ok_or("analysis_proposal_missing")?;
        if stored.confirmed { return Err("analysis_proposal_consumed".into()); }
        if now_ms() >= stored.proposal.expires_at_ms { return Err("analysis_proposal_expired".into()); }
        stored.confirmed = true;
        (stored.proposal.clone(), stored.job_path.clone(), stored.endpoint_fingerprint.clone(),
            stored.profile_epoch, Arc::clone(&stored.cancel))
    };
    let mut record = journal.read(proposal_id).map_err(|e| e.to_string())?;
    let mut run = || -> Result<crate::model_workflows::Analysis, AnalysisFlowError> {
        require_active(&cancel)?;
        let advertised = client.capabilities()?;
        let models = if proposal.models.is_empty() {
            vec![RemoteModel { capability: proposal.capability.clone(),
                graph_sha256s: proposal.graph_sha256s.clone(), model_revision: proposal.model_revision.clone() }]
        } else { proposal.models.clone() };
        for model in &models {
            let current = capability(&advertised, &model.capability)?;
            if current.graph_sha256s != model.graph_sha256s || current.model_revision != model.model_revision {
                return Err(AnalysisFlowError::CapabilityUnavailable("model identity changed"));
            }
        }
        check_gateway_limits(&advertised, &proposal.tiles)?;
        let rects: Vec<_> = proposal.tiles.iter().map(|tile| tile.rect).collect();
        let (source, width, height, tiles) = prepared_tiles(&path, proposal.page_index, TileChoice::Approved(&rects))?;
        require_unchanged(same_tiles(&proposal, &source, width, height, &tiles)
            && project_revision(&path, proposal.page_index)? == proposal.project_revision_sha256,
            "source or underlay changed before dispatch")?;
        let grants = GrantService::global();
        for model in &models {
            let grant_scope = scope(&proposal, &endpoint_fingerprint, model);
            let grant = grants.issue_grant_with_epoch(grant_scope.clone(), epoch, Duration::from_secs(300), 1)
                .map_err(|e| e.to_string())?;
            grants.validate_and_consume(&grant.nonce, &grant_scope).map_err(|e| e.to_string())?;
        }
        {
            let cache = proposals().lock().map_err(|e| e.to_string())?;
            require_active(&cancel)?;
            record.phase = AnalysisPhase::Confirmed;
            journal.write(&record).map_err(|e| e.to_string())?;
            drop(cache);
        }
        progress(&record);
        let mut all_costs_known = true;
        let mut cost = 0.0;
        let uploads: Vec<TileUpload<'_>> = tiles.iter().zip(&proposal.tiles)
            .map(|((_, png), approved)| (approved.rect, approved.png_sha256.as_str(), png.as_slice()))
            .collect();
        let record = std::cell::RefCell::new(&mut record);
        let stitched = run_tiles(client, &models, &proposal.source_page_sha256, &uploads, width, height, &cancel,
            &mut |index| {
                require_active(&cancel)?;
                require_unchanged(project_revision(&path, proposal.page_index)? == proposal.project_revision_sha256,
                    "source or underlay changed during batch")?;
                #[cfg(test)]
                {
                    let before_submit = proposals().lock().map_err(|e| e.to_string())?
                        .get(proposal_id).and_then(|stored| stored.before_submit.clone());
                    if let Some(hook) = before_submit { hook(); }
                }
                let mut record = record.borrow_mut();
                {
                    let cache = proposals().lock().map_err(|e| e.to_string())?;
                    require_active(&cancel)?;
                    record.phase = AnalysisPhase::SubmittedTile { index };
                    record.tile_submission_started = Some(true);
                    journal.write(&record).map_err(|e| e.to_string())?;
                    drop(cache);
                }
                progress(&record);
                Ok(())
            },
            &mut |index, result| {
                journal.cache_tile_result(proposal_id, index, result).map_err(|e| e.to_string())?;
                if let Some(value) = result.reported_cost_usd { cost += value; } else { all_costs_known = false; }
                let mut record = record.borrow_mut();
                record.completed_tiles = index + 1;
                record.reported_cost_usd = all_costs_known.then_some(cost);
                {
                    let cache = proposals().lock().map_err(|e| e.to_string())?;
                    record.cancel_requested = cancel.load(Ordering::SeqCst);
                    record.phase = AnalysisPhase::ResultCachedTile { index };
                    journal.write(&record).map_err(|e| e.to_string())?;
                    drop(cache);
                }
                progress(&record);
                // A gateway without job routes cannot cancel a tile it is answering: a
                // cancel during analyze then takes effect once the answer is cached.
                require_active(&cancel)
            })?;
        let record = record.into_inner();
        let Stitched { mask, boxes } = stitched;
        let (now_source, now_width, now_height, now_tiles) =
            prepared_tiles(&path, proposal.page_index, TileChoice::Approved(&rects))?;
        require_unchanged(same_tiles(&proposal, &now_source, now_width, now_height, &now_tiles),
            "source or underlay changed before evidence attachment")?;
        let _lock = run::lock_job(&path).map_err(String::from)?;
        require_unchanged(project_revision_locked(&path, proposal.page_index)? == proposal.project_revision_sha256,
            "source or underlay changed before evidence attachment")?;
        require_active(&cancel)?;
        let identity = models.iter()
            .map(|model| format!("{}:{}", model.model_revision, model.graph_sha256s.join(":")))
            .collect::<Vec<_>>().join("+");
        let evidence = crate::model_workflows::store_remote_analysis(
            proposal.provider.as_str(), &capability_label(&models), &identity,
            &source, width, height, mask, boxes,
        )?;
        record.phase = AnalysisPhase::AttachedEvidence;
        journal.write(record).map_err(|e| e.to_string())?;
        progress(record);
        Ok(evidence)
    };
    let result = run();
    if let Err(error) = &result {
        record.cancel_requested = cancel.load(Ordering::SeqCst);
        record.phase = if *error == AnalysisFlowError::Cancelled { AnalysisPhase::Cancelled }
            else if matches!(record.phase, AnalysisPhase::SubmittedTile { .. }) {
                // A later request may have reached the provider without an
                // answer. Earlier tile prices do not price the whole attempt.
                record.reported_cost_usd = None;
                AnalysisPhase::UnknownRemoteState { index: record.completed_tiles }
            } else { AnalysisPhase::Failed { code: error.to_string().split(':').next().unwrap_or("analysis_failed").to_string() } };
        let _ = journal.write(&record);
        progress(&record);
    }
    if let Ok(mut cache) = proposals().lock() {
        if let Some(stored) = cache.get_mut(proposal_id) { stored.finished = true; }
    }
    result.map_err(|e| e.to_string())
}

/// Place one tile's answer: the mask only inside the tile's core, and its
/// boxes whole, in page coordinates, for [`cloud_tiles::merge_tile_boxes`].
fn stitch(result: &AnalysisResult, core: TileRect, mask: &mut Option<Vec<u8>>,
    boxes: &mut Vec<cloud_tiles::TileBox>, page_width: u32, page_height: u32) -> Result<(), String> {
    let tile = result.tile_rect;
    if tile.x.checked_add(tile.width).is_none_or(|right| right > page_width)
        || tile.y.checked_add(tile.height).is_none_or(|bottom| bottom > page_height)
        || core.x < tile.x || core.y < tile.y
        || core.x.checked_add(core.width).is_none_or(|right| right > tile.x + tile.width)
        || core.y.checked_add(core.height).is_none_or(|bottom| bottom > tile.y + tile.height)
    { return Err("analysis result lies outside page".into()); }
    // A fused proposal sends each tile to RT-DETR as well; only SAM draws pixels.
    if let (Some(pixels), true) = (mask.as_mut(), result.capability == SAM) {
        let encoded = result.mask_bytes()?.ok_or("analysis mask missing")?;
        let decoded = decode_analysis_mask(&encoded, tile).map_err(|e| e.to_string())?;
        cloud_tiles::stitch_mask(pixels, page_width, tile, core, &decoded);
    }
    for item in &result.boxes {
        let class = match item.class {
            0 => BalloonClass::Bubble, 1 => BalloonClass::TextInBubble,
            2 => BalloonClass::TextFree, _ => return Err("invalid analysis box class".into()),
        };
        boxes.push(cloud_tiles::TileBox { tile, core, found: BalloonBox { rect: Rect::new(
            i64::from(tile.x + item.rect.x), i64::from(tile.y + item.rect.y),
            item.rect.width, item.rect.height,
        ), class, score: item.score } });
    }
    Ok(())
}

pub(crate) fn journal_for(app: &tauri::AppHandle) -> Result<AnalysisJournal, String> {
    Ok(AnalysisJournal::new(crate::inference::commands::get_app_journal_dir(app)?))
}

/// The selected profile and its client. New work goes only where the user
/// points now.
pub(crate) fn profile(
    app: &tauri::AppHandle, provider: CloudProvider, profile_id: &str,
) -> Result<(crate::inference::config::CloudProfile, CloudHttpClient), String> {
    let config = crate::inference::commands::read_inference_config(app.clone())?;
    if config.selected_target.provider() != Some(provider)
        || config.selected_target.profile_id() != Some(profile_id) {
        return Err("analysis_profile_not_active".into());
    }
    profile_in(&config, provider, profile_id)
}

/// A configured profile and its client, selected or not: for work a grant
/// already sent there.
pub(crate) fn configured_profile(
    app: &tauri::AppHandle, provider: CloudProvider, profile_id: &str,
) -> Result<(crate::inference::config::CloudProfile, CloudHttpClient), String> {
    profile_in(&crate::inference::commands::read_inference_config(app.clone())?, provider, profile_id)
}

fn profile_in(
    config: &crate::inference::config::InferenceConfig, provider: CloudProvider, profile_id: &str,
) -> Result<(crate::inference::config::CloudProfile, CloudHttpClient), String> {
    let profile = match provider {
        CloudProvider::Beam => config.beam_profiles.get(profile_id),
        CloudProvider::Modal => config.modal_profiles.get(profile_id),
    }.ok_or("analysis_profile_missing")?.clone();
    let client = crate::inference::commands::build_client_for_profile(config, provider, profile_id)
        .map_err(|e| e.to_string())?;
    Ok((profile, client))
}

#[tauri::command]
pub async fn list_remote_analysis_capabilities(
    app: tauri::AppHandle, provider: CloudProvider, profile_id: String,
) -> Result<AnalysisCapabilities, String> {
    if !crate::inference::cloud_allowed(&app) { return Err("cloud_disabled".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let (_, client) = profile(&app, provider, &profile_id)?;
        client.capabilities()
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn propose_remote_analysis(
    app: tauri::AppHandle, chapter_id: String, page_index: usize,
    provider: CloudProvider, profile_id: String, capability: String,
    companion: Option<String>, regions: Vec<TileRect>,
) -> Result<AnalysisProposal, String> {
    if !crate::inference::cloud_allowed(&app) { return Err("cloud_disabled".into()); }
    tauri::async_runtime::spawn_blocking(move || {
        let (selected, client) = profile(&app, provider, &profile_id)?;
        let path = Library::for_app(&app).map_err(|e| e.to_string())?
            .resolve_chapter(&chapter_id).map_err(|e| e.to_string())?;
        let endpoint = crate::inference::config::compute_canonical_endpoint_fingerprint(&selected.endpoint_url)?;
        propose(path, chapter_id, page_index, provider, profile_id, selected.name,
            endpoint, capability, companion, regions, &client, &journal_for(&app)?)
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn confirm_remote_analysis(
    app: tauri::AppHandle, proposal_id: String,
    rights_attested: bool, retention_acknowledged: bool,
) -> Result<crate::model_workflows::Analysis, String> {
    if !crate::inference::cloud_allowed(&app) { return Err("cloud_disabled".into()); }
    require_consent(rights_attested, retention_acknowledged).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let (provider, profile_id) = {
            let cache = proposals().lock().map_err(|e| e.to_string())?;
            let stored = cache.get(&proposal_id).ok_or("analysis_proposal_missing")?;
            (stored.proposal.provider, stored.proposal.profile_id.clone())
        };
        // Where the proposal the user consented to goes, selected now or not.
        let (_, client) = configured_profile(&app, provider, &profile_id)?;
        let journal = journal_for(&app)?;
        use tauri::Emitter;
        confirm_and_run(&proposal_id, rights_attested, retention_acknowledged,
            &client, &journal, |record| { let _ = app.emit("cloud://analysis", record); })
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel_remote_analysis(app: tauri::AppHandle, proposal_id: String) -> Result<bool, String> {
    let journal = journal_for(&app)?;
    cancel_with_journal(&proposal_id, &journal)
}

fn cancel_with_journal(proposal_id: &str, journal: &AnalysisJournal) -> Result<bool, String> {
    let cache = proposals().lock().map_err(|e| e.to_string())?;
    let mut record = journal.read(proposal_id).map_err(|e| e.to_string())?;
    if matches!(record.phase, AnalysisPhase::AttachedEvidence | AnalysisPhase::Cancelled | AnalysisPhase::Failed { .. }) {
        return Ok(false);
    }
    let active = cache.get(proposal_id).filter(|stored| !stored.finished);
    if active.is_none() {
        record.phase = if matches!(record.phase, AnalysisPhase::Proposed) {
            AnalysisPhase::Cancelled
        } else {
            journal.recover(proposal_id).map_err(|e| e.to_string())?.phase
        };
    } else if matches!(record.phase, AnalysisPhase::Proposed) {
        record.phase = AnalysisPhase::Cancelled;
    }
    record.cancel_requested = true;
    journal.write(&record).map_err(|e| e.to_string())?;
    if let Some(stored) = active { stored.cancel.store(true, Ordering::SeqCst); }
    Ok(true)
}

#[tauri::command]
pub fn get_remote_analysis_status(
    app: tauri::AppHandle, proposal_id: String,
) -> Result<AnalysisAttemptRecord, String> {
    let journal = journal_for(&app)?;
    let active = proposals().lock().map_err(|e| e.to_string())?.contains_key(&proposal_id);
    if active { journal.read(&proposal_id) } else { journal.recover(&proposal_id) }
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use cleaner_core::cloud_analysis_wire::{AnalysisCapability, AnalysisLimits, AnalysisTimings};
    use cleaner_core::image::{fixtures, Format};
    use cleaner_core::mask::Mask;
    use cleaner_core::patch::{Engine, Patch, Provenance};
    use cleaner_core::project::Project;
    use base64::Engine as _;

    #[test]
    fn analysis_transport_matches_independent_cmyk_reference_and_declares_srgb() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../crates/cleaner-core/tests/fixtures/color-reference");
        let page = cleaner_core::image::decode(&std::fs::read(root.join("cmyk-adobe-icc.jpg")).unwrap()).unwrap();
        let expected = std::fs::read(root.join("cmyk-adobe.srgb")).unwrap();
        let (_, bytes) = encode_tile(&page, TileRect { x: 0, y: 0, width: page.width, height: page.height }).unwrap();
        let transport = cleaner_core::image::decode(&bytes).unwrap();
        assert_eq!(transport.srgb_intent, Some(1));
        assert!(transport.icc.is_none());
        assert_eq!(transport.data.len(), expected.len());
        for (actual, expected) in transport.data.iter().zip(expected) {
            assert!(actual.abs_diff(expected) <= 3, "{actual} vs {expected}");
        }
    }

    #[test]
    fn analysis_transport_flattens_transparency_over_white_after_color_management() {
        let mut page = fixtures::by_name("rgba8").raster;
        page.width = 2; page.height = 1;
        page.data = vec![0, 0, 0, 128, 255, 0, 0, 0];
        page.color.gamma = Some(100000);
        let (_, bytes) = encode_tile(&page, TileRect { x: 0, y: 0, width: 2, height: 1 }).unwrap();
        let actual = cleaner_core::image::decode(&bytes).unwrap();
        assert!(actual.data[..3].iter().all(|&v| (187..=189).contains(&v)));
        assert_eq!(&actual.data[3..], &[255, 255, 255]);
        assert_eq!(page.data, [0, 0, 0, 128, 255, 0, 0, 0]);
    }

    struct FakeGateway {
        submissions: AtomicUsize,
        change_predecessor: Option<PathBuf>,
        cancel_after_first: Mutex<Option<String>>,
    }

    impl AnalysisGateway for FakeGateway {
        fn capabilities(&self) -> Result<AnalysisCapabilities, String> {
            Ok(AnalysisCapabilities {
                protocol_version: VERSION.into(),
                capabilities: vec![AnalysisCapability {
                    capability: SAM.into(), graph_sha256s: vec!["a".repeat(64), "b".repeat(64)],
                    model_revision: "c".repeat(40),
                }],
                limits: AnalysisLimits {
                    max_tile_side: 1024, max_tile_pixels: 1_048_576,
                    max_png_bytes: 4_194_304, max_components: 4096, max_boxes: 4096,
                },
            })
        }

        fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String> {
            request.validate(png)?;
            let count = self.submissions.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                if let Some(id) = self.cancel_after_first.lock().unwrap().as_ref() {
                    cancel(id)?;
                }
            }
            if let Some(path) = &self.change_predecessor {
                let mut job = Job::open(path).map_err(|e| e.to_string())?;
                job.project.patches[0].visible = false;
                job.flush().map_err(|e| e.to_string())?;
            }
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, request.tile_rect.width, request.tile_rect.height);
                encoder.set_color(png::ColorType::Grayscale);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.write_header().map_err(|e| e.to_string())?
                    .write_image_data(&vec![255; (request.tile_rect.width * request.tile_rect.height) as usize])
                    .map_err(|e| e.to_string())?;
            }
            let result = AnalysisResult {
                protocol_version: VERSION.into(), capability: request.capability.clone(),
                request_digest: request.request_digest.clone(), tile_id: request.tile_id.clone(),
                tile_rect: request.tile_rect, graph_sha256s: request.graph_sha256s.clone(),
                model_revision: request.model_revision.clone(),
                mask_png_b64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
                components: vec![], boxes: vec![],
                timings: AnalysisTimings { load_ms: 0, preprocess_ms: 0, inference_ms: 0, postprocess_ms: 0 },
                reported_cost_usd: None,
            };
            result.validate(request)?;
            Ok(result)
        }
    }

    /// Answers batches only, each request through [`FakeGateway::analyze`].
    struct Batching {
        inner: FakeGateway,
        batches: Mutex<Vec<(usize, usize)>>,
    }

    impl AnalysisGateway for Batching {
        fn capabilities(&self) -> Result<AnalysisCapabilities, String> { self.inner.capabilities() }
        fn analyze(&self, _: &AnalysisRequest, _: &[u8]) -> Result<AnalysisResult, String> {
            Err("a tile sent to a gateway that serves batches".into())
        }
        fn serves_batches(&self) -> Result<bool, String> { Ok(true) }
        fn analyze_batch(&self, batch: &AnalysisBatch, tiles: &[&[u8]], _: &AtomicBool)
            -> Result<AnalysisBatchResult, String> {
            self.batches.lock().unwrap().push((batch.requests.len(), tiles.len()));
            let results = batch.requests.iter().map(|request| {
                let png = tiles.iter().find(|png| hex(png) == request.tile_png_sha256).ok_or("tile not sent")?;
                self.inner.analyze(request, png)
            }).collect::<Result<_, String>>()?;
            Ok(AnalysisBatchResult { protocol_version: VERSION.into(), request_digest: batch.request_digest.clone(),
                results })
        }
    }

    fn fake_gateway() -> FakeGateway {
        FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None, cancel_after_first: Mutex::new(None) }
    }

    /// A gateway with batches gets a page's tiles in one call, each PNG once,
    /// and the page stitches to what the tile-by-tile loop makes of it.
    #[test]
    fn a_page_goes_to_a_batching_gateway_in_one_call() {
        let mut page = fixtures::by_name("l8").raster;
        (page.width, page.height) = (1500, 256);
        page.data = (0..1500 * 256).map(|i| (i % 251) as u8).collect();
        let encoded: Vec<(TileRect, String, Vec<u8>)> = cloud_tiles::plan(1500, 256, &[]).unwrap().tiles.iter()
            .map(|rect| { let (sha, png) = encode_tile(&page, *rect).unwrap(); (*rect, sha, png) }).collect();
        assert_eq!(encoded.len(), 2);
        let uploads: Vec<TileUpload<'_>> = encoded.iter().map(|(rect, sha, png)| (*rect, sha.as_str(), png.as_slice())).collect();
        let models = vec![RemoteModel { capability: SAM.into(), graph_sha256s: vec!["a".repeat(64), "b".repeat(64)],
            model_revision: "c".repeat(40) }];
        let run = |client: &dyn AnalysisGateway| {
            let (mut before, mut after) = (Vec::new(), Vec::new());
            let stitched = run_tiles(client, &models, &"e".repeat(64), &uploads, 1500, 256, &AtomicBool::new(false),
                &mut |step| { before.push(step); Ok(()) }, &mut |step, _| { after.push(step); Ok(()) }).unwrap();
            (stitched.mask, before, after)
        };
        let batching = Batching { inner: fake_gateway(), batches: Mutex::new(Vec::new()) };
        let (batched, before, after) = run(&batching);
        assert_eq!(*batching.batches.lock().unwrap(), vec![(2, 2)]);
        assert_eq!((before, after), (vec![0, 1], vec![0, 1]));
        let one_by_one = fake_gateway();
        let (sequential, ..) = run(&one_by_one);
        assert_eq!(one_by_one.submissions.load(Ordering::SeqCst), 2);
        assert!(batched.is_some() && batched == sequential);
    }

    fn fixture(name: &str) -> (PathBuf, AnalysisJournal) {
        fixture_with_width(name, 256)
    }

    fn fixture_with_width(name: &str, width: u32) -> (PathBuf, AnalysisJournal) {
        let root = std::env::temp_dir().join(format!("mc-analysis-{name}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let mut page = fixtures::by_name("l8").raster;
        page.width = width;
        page.height = 256;
        page.data = vec![200; width as usize * 256];
        let source = root.join("page.png");
        let bytes = cleaner_core::image::encode(&page, Format::Png).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        let reference = cleaner_core::ingest::source_ref(&source, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(manifest.parent().unwrap(), "test-app", StripMode::Single, &[reference]);
        let mut job = Job::create(&manifest, project).unwrap();
        let bounds = Rect::new(20, 20, 16, 16);
        let mut pixels = page;
        pixels.width = 16;
        pixels.height = 16;
        pixels.data = vec![100; 16 * 16];
        let patch = Patch {
            id: "earlier".into(), mask: Mask::filled(bounds), ink: Mask::filled(bounds),
            pixels, order: 1, visible: true,
            provenance: Provenance {
                engine: Engine::Fill, engine_version: "1.0".into(), model_sha256: None,
                execution_provider: "cpu".into(), params_snapshot: serde_json::json!({}),
                mask_sha256: hex(&vec![255; 16 * 16]), source_sha256: hex(&bytes),
                cloud: None, created: 1_760_000_000,
            },
        };
        job.complete_region(0, &patch, None).unwrap();
        let journal = AnalysisJournal::new(root.join("journal"));
        (manifest, journal)
    }

    fn proposal(path: PathBuf, journal: &AnalysisJournal, gateway: &FakeGateway) -> AnalysisProposal {
        propose(path, "chapter".into(), 0, CloudProvider::Modal, "modal-prof-1".into(),
            "Modal".into(), "a".repeat(64), SAM.into(), None, vec![], gateway, journal).unwrap()
    }

    struct PricedThenLost {
        inner: FakeGateway,
        calls: AtomicUsize,
    }

    impl AnalysisGateway for PricedThenLost {
        fn capabilities(&self) -> Result<AnalysisCapabilities, String> { self.inner.capabilities() }
        fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err("transport_lost".into());
            }
            let mut result = self.inner.analyze(request, png)?;
            result.reported_cost_usd = Some(0.25);
            Ok(result)
        }
    }

    #[test]
    fn priced_first_tile_then_unknown_second_records_no_cost() {
        let (path, journal) = fixture_with_width("priced-then-lost", 1200);
        let gateway = PricedThenLost {
            inner: FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
                cancel_after_first: Mutex::new(None) },
            calls: AtomicUsize::new(0),
        };
        let approved = propose(path.clone(), "chapter".into(), 0, CloudProvider::Modal,
            "modal-prof-priced-lost".into(), "Modal".into(), "a".repeat(64),
            SAM.into(), None, vec![], &gateway, &journal).unwrap();
        assert_eq!(approved.tiles.len(), 2);
        assert!(confirm_and_run(&approved.proposal_id, true, true, &gateway, &journal, |_| {}).is_err());
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
        let record = journal.read(&approved.proposal_id).unwrap();
        assert!(matches!(record.phase, AnalysisPhase::UnknownRemoteState { index: 1 }));
        assert_eq!(record.completed_tiles, 1);
        assert_eq!(record.reported_cost_usd, None);
    }

    #[test]
    fn analysis_consent_submit_and_review_evidence_stay_review_only() {
        let (path, journal) = fixture("happy");
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let proposal = proposal(path, &journal, &gateway);
        assert_eq!(proposal.pages, 1);
        assert!(proposal.includes_surrounding_art);
        assert_eq!(proposal.cost_estimate_usd, None);
        assert_eq!(require_consent(false, true).unwrap_err(), AnalysisFlowError::RightsAttestationRequired);
        assert_eq!(require_consent(true, false).unwrap_err(), AnalysisFlowError::RetentionAcknowledgementRequired);
        assert_eq!(confirm_and_run(&proposal.proposal_id, false, true, &gateway, &journal, |_| {}).err().unwrap(),
            AnalysisFlowError::RightsAttestationRequired.to_string());
        assert_eq!(confirm_and_run(&proposal.proposal_id, true, false, &gateway, &journal, |_| {}).err().unwrap(),
            AnalysisFlowError::RetentionAcknowledgementRequired.to_string());
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 0);
        let evidence = confirm_and_run(&proposal.proposal_id, true, true, &gateway, &journal, |_| {}).unwrap();
        let value = serde_json::to_value(evidence).unwrap();
        assert_eq!(value["samWriteEligible"], false);
        assert_eq!(value["remoteSource"], "remote:modal:text_mask_sam_ts@1");
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 1);
        assert!(matches!(journal.read(&proposal.proposal_id).unwrap().phase, AnalysisPhase::AttachedEvidence));
    }

    /// A partial selection is confirmed as approved: one tile of a 1600 wide
    /// page, whose rectangle reaches into the other tile's core, is sent
    /// alone and is not mistaken for a changed page.
    #[test]
    fn a_partial_selection_confirms_exactly_the_approved_tiles() {
        let (path, journal) = fixture_with_width("partial", 1600);
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let region = TileRect { x: 10, y: 10, width: 50, height: 50 };
        let proposal = propose(path, "chapter".into(), 0, CloudProvider::Modal, "modal-prof-1".into(),
            "Modal".into(), "a".repeat(64), SAM.into(), None, vec![region], &gateway, &journal).unwrap();
        assert_eq!(proposal.tiles.iter().map(|t| t.rect).collect::<Vec<_>>(),
            vec![TileRect { x: 0, y: 0, width: 1024, height: 256 }]);
        confirm_and_run(&proposal.proposal_id, true, true, &gateway, &journal, |_| {}).unwrap();
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 1);
        assert!(matches!(journal.read(&proposal.proposal_id).unwrap().phase, AnalysisPhase::AttachedEvidence));
    }

    #[test]
    fn predecessor_change_after_submission_stales_without_second_job() {
        let (path, journal) = fixture_with_width("stale", 1300);
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: Some(path.clone()),
            cancel_after_first: Mutex::new(None) };
        let proposal = proposal(path, &journal, &gateway);
        assert_eq!(proposal.tiles.len(), 2);
        assert_eq!(require_unchanged(false, "source or underlay changed during batch").unwrap_err(),
            AnalysisFlowError::Stale("source or underlay changed during batch"));
        let error = confirm_and_run(&proposal.proposal_id, true, true, &gateway, &journal, |_| {}).err().unwrap();
        assert_eq!(error, AnalysisFlowError::Stale("source or underlay changed during batch").to_string());
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 1);
        assert_eq!(journal.read(&proposal.proposal_id).unwrap().completed_tiles, 1);
        assert!(!matches!(journal.read(&proposal.proposal_id).unwrap().phase, AnalysisPhase::AttachedEvidence));
    }

    #[test]
    fn cloud_unconfigured_does_not_affect_local_analysis_contract() {
        let config = crate::inference::config::InferenceConfig::default();
        assert!(config.beam_profiles.is_empty() && config.modal_profiles.is_empty());
        let page = fixtures::by_name("l8").raster;
        let mask = vec![0; (page.width * page.height) as usize];
        assert!(cleaner_core::fusion::fuse(page.width, page.height, Some(&mask), &[], &[]).is_ok());
    }

    #[test]
    fn missing_capability_and_cancel_never_submit_a_tile() {
        let (path, journal) = fixture("refusal");
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let missing = propose(path.clone(), "chapter".into(), 0, CloudProvider::Modal,
            "modal-prof-1".into(), "Modal".into(), "a".repeat(64), RT.into(),
            None, vec![], &gateway, &journal);
        assert_eq!(capability(&gateway.capabilities().unwrap(), RT).err().unwrap(),
            AnalysisFlowError::CapabilityUnavailable("gateway does not advertise this analysis model"));
        assert_eq!(missing.unwrap_err(), AnalysisFlowError::CapabilityUnavailable(
            "gateway does not advertise this analysis model").to_string());
        let approved = proposal(path, &journal, &gateway);
        assert!(cancel(&approved.proposal_id).unwrap());
        assert_eq!(require_active(&proposals().lock().unwrap().get(&approved.proposal_id).unwrap().cancel)
            .unwrap_err(), AnalysisFlowError::Cancelled);
        assert_eq!(confirm_and_run(&approved.proposal_id, true, true, &gateway, &journal, |_| {}).err().unwrap(),
            AnalysisFlowError::Cancelled.to_string());
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 0);
        let record = journal.read(&approved.proposal_id).unwrap();
        assert!(matches!(record.phase, AnalysisPhase::Cancelled));
        assert_eq!(record.tile_submission_started, Some(false));
    }

    #[test]
    fn cancellation_after_first_tile_stops_remaining_batch() {
        let (path, journal) = fixture_with_width("cancel-batch", 1300);
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let approved = proposal(path, &journal, &gateway);
        assert_eq!(approved.tiles.len(), 2);
        *gateway.cancel_after_first.lock().unwrap() = Some(approved.proposal_id.clone());
        let error = confirm_and_run(&approved.proposal_id, true, true, &gateway, &journal, |_| {})
            .err().unwrap();
        assert_eq!(error, "analysis_cancelled");
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 1);
        let record = journal.read(&approved.proposal_id).unwrap();
        assert!(matches!(record.phase, AnalysisPhase::Cancelled));
        assert_eq!(record.completed_tiles, 1);
        assert_eq!(record.tile_submission_started, Some(true));
    }

    #[test]
    fn live_cancel_between_tiles_never_sends_next_tile() {
        let (path, journal) = fixture_with_width("cancel-between-tiles", 1300);
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let approved = proposal(path, &journal, &gateway);
        let id = approved.proposal_id.clone();
        let error = confirm_and_run(&id, true, true, &gateway, &journal, |record| {
            if matches!(record.phase, AnalysisPhase::ResultCachedTile { index: 0 }) {
                cancel(&id).unwrap();
            }
        }).err().unwrap();
        assert_eq!(error, AnalysisFlowError::Cancelled.to_string());
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 1);
        let record = journal.read(&id).unwrap();
        assert!(matches!(record.phase, AnalysisPhase::Cancelled));
        assert!(record.cancel_requested);
        assert_eq!(record.completed_tiles, 1);
    }

    #[test]
    fn cancel_before_submitted_mark_prevents_dispatch() {
        let (path, journal) = fixture_with_width("cancel-before-mark", 1300);
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let approved = proposal(path, &journal, &gateway);
        let id = approved.proposal_id.clone();
        let reached = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        proposals().lock().unwrap().get_mut(&id).unwrap().before_submit = Some(Arc::new({
            let reached = Arc::clone(&reached);
            let resume = Arc::clone(&resume);
            move || { reached.wait(); resume.wait(); }
        }));
        std::thread::scope(|scope| {
            let run = scope.spawn(|| confirm_and_run(&id, true, true, &gateway, &journal, |_| {}));
            reached.wait();
            assert!(cancel_with_journal(&id, &journal).unwrap());
            let record = journal.read(&id).unwrap();
            assert!(record.cancel_requested);
            assert!(matches!(record.phase, AnalysisPhase::Confirmed));
            resume.wait();
            assert_eq!(run.join().unwrap().err().unwrap(), "analysis_cancelled");
        });
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 0);
        let record = journal.read(&id).unwrap();
        assert!(matches!(record.phase, AnalysisPhase::Cancelled));
        assert_eq!(record.tile_submission_started, Some(false));
    }

    #[test]
    fn cancel_after_submitted_mark_reports_one_in_flight_tile() {
        struct BlockingGateway<'a> {
            inner: &'a FakeGateway,
            entered: Arc<std::sync::Barrier>,
            resume: Arc<std::sync::Barrier>,
        }
        impl AnalysisGateway for BlockingGateway<'_> {
            fn capabilities(&self) -> Result<AnalysisCapabilities, String> { self.inner.capabilities() }
            fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String> {
                self.entered.wait();
                self.resume.wait();
                self.inner.analyze(request, png)
            }
        }
        let (path, journal) = fixture_with_width("cancel-after-mark", 1300);
        let inner = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let approved = proposal(path, &journal, &inner);
        assert_eq!(approved.tiles.len(), 2);
        let id = approved.proposal_id.clone();
        let entered = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let gateway = BlockingGateway { inner: &inner, entered: Arc::clone(&entered), resume: Arc::clone(&resume) };
        std::thread::scope(|scope| {
            let run = scope.spawn(|| confirm_and_run(&id, true, true, &gateway, &journal, |_| {}));
            entered.wait();
            assert!(cancel_with_journal(&id, &journal).unwrap());
            let record = journal.read(&id).unwrap();
            assert!(record.cancel_requested);
            assert!(matches!(record.phase, AnalysisPhase::SubmittedTile { index: 0 }));
            assert_eq!(record.completed_tiles, 0);
            resume.wait();
            assert_eq!(run.join().unwrap().err().unwrap(), "analysis_cancelled");
        });
        assert_eq!(inner.submissions.load(Ordering::SeqCst), 1);
        let record = journal.read(&id).unwrap();
        assert!(record.cancel_requested);
        assert!(matches!(record.phase, AnalysisPhase::Cancelled));
        assert_eq!(record.completed_tiles, 1);
        assert_eq!(record.tile_submission_started, Some(true));
    }

    /// On a gateway that polls, a cancel while a tile is out gives the tile up
    /// at once (the HTTP client cancels it on the gateway) instead of waiting for
    /// its answer: the batch ends cancelled with no tile completed.
    #[test]
    fn cancel_gives_up_the_tile_in_flight_on_a_polling_gateway() {
        struct PollingGateway<'a> {
            inner: &'a FakeGateway,
            entered: Arc<std::sync::Barrier>,
        }
        impl AnalysisGateway for PollingGateway<'_> {
            fn capabilities(&self) -> Result<AnalysisCapabilities, String> { self.inner.capabilities() }
            fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String> {
                self.inner.analyze(request, png)
            }
            fn analyze_cancellable(&self, _: &AnalysisRequest, _: &[u8], cancel: &AtomicBool)
                -> Result<AnalysisResult, String> {
                self.inner.submissions.fetch_add(1, Ordering::SeqCst);
                self.entered.wait();
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !cancel.load(Ordering::SeqCst) {
                    assert!(std::time::Instant::now() < deadline, "the cancel never reached the tile");
                    std::thread::yield_now();
                }
                Err(HttpTransportError::JobCancelled.to_string())
            }
        }
        let (path, journal) = fixture_with_width("cancel-polling", 1300);
        let inner = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let approved = proposal(path, &journal, &inner);
        let id = approved.proposal_id.clone();
        let entered = Arc::new(std::sync::Barrier::new(2));
        let gateway = PollingGateway { inner: &inner, entered: Arc::clone(&entered) };
        std::thread::scope(|scope| {
            let run = scope.spawn(|| confirm_and_run(&id, true, true, &gateway, &journal, |_| {}));
            entered.wait();
            assert!(cancel_with_journal(&id, &journal).unwrap());
            assert_eq!(run.join().unwrap().err().unwrap(), "analysis_cancelled");
        });
        assert_eq!(inner.submissions.load(Ordering::SeqCst), 1, "no tile after the cancel");
        let record = journal.read(&id).unwrap();
        assert!(record.cancel_requested);
        assert!(matches!(record.phase, AnalysisPhase::Cancelled));
        assert_eq!(record.completed_tiles, 0);
    }

    #[test]
    fn cancelling_active_proposed_proposal_persists_cancelled() {
        let (path, journal) = fixture("cancel-proposed");
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let approved = proposal(path, &journal, &gateway);
        assert!(cancel_with_journal(&approved.proposal_id, &journal).unwrap());
        let record = journal.read(&approved.proposal_id).unwrap();
        assert!(record.cancel_requested);
        assert!(matches!(record.phase, AnalysisPhase::Cancelled));
        assert_eq!(record.tile_submission_started, Some(false));
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn recovered_submitted_tile_discard_keeps_remote_outcome_unknown() {
        let (path, journal) = fixture("recovered-cancel");
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let approved = proposal(path, &journal, &gateway);
        let mut record = journal.read(&approved.proposal_id).unwrap();
        record.phase = AnalysisPhase::SubmittedTile { index: 0 };
        journal.write(&record).unwrap();
        proposals().lock().unwrap().remove(&approved.proposal_id);

        assert!(cancel_with_journal(&approved.proposal_id, &journal).unwrap());
        let persisted = journal.read(&approved.proposal_id).unwrap();
        assert!(persisted.cancel_requested);
        assert!(matches!(persisted.phase, AnalysisPhase::UnknownRemoteState { index: 0 }));
        assert!(matches!(journal.recover(&approved.proposal_id).unwrap().phase,
            AnalysisPhase::UnknownRemoteState { index: 0 }));
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stitch_places_distinct_masks_on_partial_edge_tile() {
        fn tile_result(rect: TileRect, set: &[(u32, u32)]) -> AnalysisResult {
            let mut pixels = vec![0u8; (rect.width * rect.height) as usize];
            for &(x, y) in set { pixels[(y * rect.width + x) as usize] = 255; }
            let mut png_bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut png_bytes, rect.width, rect.height);
                encoder.set_color(png::ColorType::Grayscale);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
            }
            AnalysisResult {
                protocol_version: VERSION.into(), capability: SAM.into(),
                request_digest: "a".repeat(64), tile_id: "tile_0".into(), tile_rect: rect,
                graph_sha256s: vec!["b".repeat(64), "c".repeat(64)],
                model_revision: "d".repeat(40),
                mask_png_b64: Some(base64::engine::general_purpose::STANDARD.encode(png_bytes)),
                components: vec![], boxes: vec![],
                timings: AnalysisTimings { load_ms: 0, preprocess_ms: 0, inference_ms: 0, postprocess_ms: 0 },
                reported_cost_usd: None,
            }
        }

        // Two overlapping tiles; each keeps only its core, so a pixel set in
        // the overlap by the tile that does not own it never reaches the page.
        let mut mask = Some(vec![0u8; 5 * 3]);
        let mut boxes = Vec::new();
        let left = TileRect { x: 0, y: 0, width: 4, height: 3 };
        let right = TileRect { x: 1, y: 0, width: 4, height: 3 };
        stitch(&tile_result(left, &[(0, 0), (2, 2), (3, 1)]),
            TileRect { x: 0, y: 0, width: 3, height: 3 }, &mut mask, &mut boxes, 5, 3).unwrap();
        stitch(&tile_result(right, &[(0, 0), (2, 1), (3, 2)]),
            TileRect { x: 3, y: 0, width: 2, height: 3 }, &mut mask, &mut boxes, 5, 3).unwrap();
        assert_eq!(mask.as_ref().unwrap().as_slice(), &[
            255, 0, 0, 0, 0,
            0, 0, 0, 255, 0,
            0, 0, 255, 0, 255,
        ]);
        assert_eq!(mask.as_ref().unwrap().len(), 15);
        assert!(stitch(&tile_result(TileRect { x: 4, y: 0, width: 2, height: 3 }, &[(1, 0)]),
            TileRect { x: 4, y: 0, width: 2, height: 3 }, &mut mask, &mut boxes, 5, 3).is_err());
        assert!(stitch(&tile_result(left, &[]), TileRect { x: 0, y: 0, width: 5, height: 3 },
            &mut mask, &mut boxes, 5, 3).is_err(), "a core outside its tile");
        assert!(boxes.is_empty());
    }

    #[test]
    fn expired_proposal_cannot_mint_an_analysis_grant() {
        let (path, journal) = fixture("expired");
        let gateway = FakeGateway { submissions: AtomicUsize::new(0), change_predecessor: None,
            cancel_after_first: Mutex::new(None) };
        let approved = proposal(path, &journal, &gateway);
        proposals().lock().unwrap().get_mut(&approved.proposal_id).unwrap()
            .proposal.expires_at_ms = 0;
        let error = confirm_and_run(&approved.proposal_id, true, true, &gateway, &journal, |_| {}).err().unwrap();
        assert!(matches!(error.as_str(), "analysis_proposal_expired" | "analysis_proposal_missing"));
        assert_eq!(gateway.submissions.load(Ordering::SeqCst), 0);
    }
}
