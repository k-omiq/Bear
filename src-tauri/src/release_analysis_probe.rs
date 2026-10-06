//! Debug-only analysis capture on the five copied demo originals.
//! Prepare records exact consent terms; execute re-prepares and confirms them.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use base64::Engine;
use cleaner_core::cloud_analysis_wire::{AnalysisRequest, AnalysisResult, SAM, VERSION};
use cleaner_core::engines::render::{CloudProvider, ExecutionTarget};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::project::Job;
use serde_json::{json, Value};
use tauri::Manager;

use crate::inference::analysis::{self, AnalysisGateway, AnalysisProposal};
use crate::inference::config::{compute_canonical_endpoint_fingerprint, read_inference_config};
use crate::inference::http::CloudHttpClient;
use crate::inference::policy::GrantService;
use crate::library::Library;

const APP_ID: &str = "com.mangacleaner.validation20260927";
const APP_DATA: &str =
    "/Users/caved/Library/Application Support/com.mangacleaner.validation20260927";
const PAGES: [(&str, &str); 5] = [
    (
        "01",
        "2d5705a1ddf9542a235bfa740ac07e712ce8152fbb4387940c4599a0e5565e24",
    ),
    (
        "03",
        "396f8ab96d19142eef2c8c2730d1c07f0031df8d504ec698f14fcdf63c9b58b3",
    ),
    (
        "04",
        "f0cad33c870656d50e34965e7193c3a26010ef95a1d94e25480d77542277370a",
    ),
    (
        "05",
        "9cc12b397d9ad143ab9e7ee54eda16854f00b9a9a700b07edd7362b631a4d0f1",
    ),
    (
        "21",
        "589f71b718384407ca8faa8ca585b2e518519ba37281bbce0ca6a6b734617fc8",
    ),
];
const MAX_POSTS: usize = 20;

struct Captured {
    request: AnalysisRequest,
    result: AnalysisResult,
    elapsed_ms: u128,
}

struct CaptureGateway {
    client: CloudHttpClient,
    app: tauri::AppHandle,
    profile_id: String,
    endpoint_fingerprint: String,
    profile_epoch: u64,
    count: AtomicUsize,
    records: Mutex<Vec<Captured>>,
}

impl CaptureGateway {
    fn new(client: CloudHttpClient, app: tauri::AppHandle, profile_id: String,
        endpoint_fingerprint: String) -> Self {
        let profile_epoch = GrantService::global().get_profile_epoch(CloudProvider::Modal, &profile_id);
        Self {
            client,
            app,
            profile_id,
            endpoint_fingerprint,
            profile_epoch,
            count: AtomicUsize::new(0),
            records: Mutex::new(Vec::new()),
        }
    }

    fn check_authority(&self) -> Result<(), String> {
        if !crate::inference::cloud_allowed(&self.app) {
            return Err("cloud disabled before analysis upload".into());
        }
        let config = read_inference_config(&self.app)
            .map_err(|_| "analysis profile unavailable before upload")?;
        if config.selected_target.provider() != Some(CloudProvider::Modal)
            || config.selected_target.profile_id() != Some(self.profile_id.as_str()) {
            return Err("analysis profile changed before upload".into());
        }
        let profile = config.modal_profiles.get(&self.profile_id)
            .ok_or("analysis profile unavailable before upload")?;
        let current = compute_canonical_endpoint_fingerprint(&profile.endpoint_url)
            .map_err(|_| "analysis endpoint unavailable before upload")?;
        if current != self.endpoint_fingerprint
            || GrantService::global().get_profile_epoch(CloudProvider::Modal, &self.profile_id)
                != self.profile_epoch {
            return Err("analysis endpoint or profile changed before upload".into());
        }
        Ok(())
    }
}

impl AnalysisGateway for CaptureGateway {
    fn capabilities(
        &self,
    ) -> Result<cleaner_core::cloud_analysis_wire::AnalysisCapabilities, String> {
        AnalysisGateway::capabilities(&self.client)
    }

    fn analyze(&self, request: &AnalysisRequest, png: &[u8]) -> Result<AnalysisResult, String> {
        self.check_authority()?;
        let next = self.count.fetch_add(1, Ordering::SeqCst);
        if next >= MAX_POSTS || request.capability != SAM {
            return Err("analysis probe request cap reached".into());
        }
        let began = Instant::now();
        let result = AnalysisGateway::analyze(&self.client, request, png)?;
        self.records
            .lock()
            .map_err(|_| "analysis capture unavailable")?
            .push(Captured {
                request: request.clone(),
                result: result.clone(),
                elapsed_ms: began.elapsed().as_millis(),
            });
        Ok(result)
    }

    fn release_idle(&self) -> Result<(), String> {
        AnalysisGateway::release_idle(&self.client)
    }
}

pub fn main() -> Result<(), String> {
    run()
}

fn run() -> Result<(), String> {
    let mode = std::env::var("MC_RELEASE_ANALYSIS_MODE").map_err(|_| "set analysis probe mode")?;
    if !matches!(mode.as_str(), "prepare" | "execute") {
        return Err("analysis probe mode must be prepare or execute".into());
    }
    if mode == "execute" && std::env::var("MC_RELEASE_ANALYSIS_EXECUTE").as_deref() != Ok("1") {
        return Err("set MC_RELEASE_ANALYSIS_EXECUTE=1 for the approved batch".into());
    }
    if mode == "execute"
        && (std::env::var("MC_RELEASE_ANALYSIS_RIGHTS_ATTESTED").as_deref() != Ok("1")
            || std::env::var("MC_RELEASE_ANALYSIS_RETENTION_ACKNOWLEDGED").as_deref() != Ok("1"))
    {
        return Err("set explicit rights and retention acknowledgements for analysis".into());
    }
    let report = PathBuf::from(
        std::env::var("MC_RELEASE_ANALYSIS_REPORT")
            .map_err(|_| "set absolute analysis report path")?,
    );
    if !report.is_absolute() {
        return Err("analysis report path must be absolute".into());
    }
    let output = PathBuf::from(
        std::env::var("MC_RELEASE_ANALYSIS_OUTPUT")
            .map_err(|_| "set absolute fresh analysis output directory")?,
    );
    if !output.is_absolute() {
        return Err("analysis output path must be absolute".into());
    }
    crate::release_probe::validate_release_artifact_path(&report)?;
    crate::release_probe::validate_release_artifact_path(&output)?;
    let chapter_id = std::env::var("MC_RELEASE_ANALYSIS_CHAPTER_ID")
        .map_err(|_| "set copied five-page analysis chapter id")?;
    let mut context = crate::app_context();
    if context.config().identifier != APP_ID {
        return Err("validation app identifier required".into());
    }
    context.config_mut().app.windows.clear();
    let app = tauri::Builder::default()
        .build(context)
        .map_err(|_| "validation app could not start")?;
    let handle = app.handle();
    if handle
        .path()
        .app_data_dir()
        .map_err(|_| "validation app path unavailable")?
        != Path::new(APP_DATA)
    {
        return Err("validation app data path required".into());
    }
    if !crate::inference::cloud_allowed(handle) {
        return Err("cloud is disabled in validation app".into());
    }
    let library = Library::for_app(handle).map_err(|_| "validation library unavailable")?;
    let job_path = library
        .resolve_chapter(&chapter_id)
        .map_err(|_| "validation chapter unavailable")?;
    if !job_path
        .canonicalize()
        .map_err(|_| "validation chapter unavailable")?
        .starts_with(Path::new(APP_DATA))
    {
        return Err("chapter is outside validation app data".into());
    }
    let job = Job::open(&job_path).map_err(|_| "validation chapter unreadable")?;
    if job.project.sources.len() != PAGES.len()
        || !job.project.detections.is_empty()
        || !job.project.patches.is_empty()
    {
        return Err("expected untouched five-page validation chapter".into());
    }
    for (index, (_, expected)) in PAGES.iter().enumerate() {
        let source_idx =
            Library::resolve_page(&job.project, index).ok_or("validation page unavailable")?;
        let source = job
            .source_path(source_idx)
            .ok_or("validation source unavailable")?;
        if !source
            .canonicalize()
            .map_err(|_| "validation source unavailable")?
            .starts_with(Path::new(APP_DATA))
        {
            return Err("source is outside validation app data".into());
        }
        if sha256_hex(&fs::read(source).map_err(|_| "validation source unreadable")?) != *expected {
            return Err("validation source hash differs from annotated original".into());
        }
    }
    let config =
        read_inference_config(handle).map_err(|_| "validation cloud configuration unavailable")?;
    let profile_id = match &config.selected_target {
        ExecutionTarget::Modal { profile_id } => profile_id.clone(),
        _ => return Err("select Modal in validation app".into()),
    };
    let (selected, client) = analysis::profile(handle, CloudProvider::Modal, &profile_id)
        .map_err(|_| "validation runtime credential unavailable")?;
    let endpoint_fingerprint = compute_canonical_endpoint_fingerprint(&selected.endpoint_url)
        .map_err(|_| "validation endpoint unavailable")?;
    let capabilities = client
        .get_analysis_capabilities()
        .map_err(|_| "analysis capability handshake failed")?;
    if capabilities.protocol_version != VERSION
        || !capabilities
            .capabilities
            .iter()
            .any(|entry| entry.capability == SAM)
    {
        return Err("matching remote SAM capability required".into());
    }
    let mut proposals = Vec::new();
    for index in 0..PAGES.len() {
        proposals.push(
            tauri::async_runtime::block_on(analysis::propose_remote_analysis(
                handle.clone(),
                chapter_id.clone(),
                index,
                CloudProvider::Modal,
                profile_id.clone(),
                SAM.into(),
                None,
                Vec::new(),
            ))
            .map_err(|_| "production analysis proposal failed")?,
        );
    }
    let terms = approval_terms(&chapter_id, &profile_id, &endpoint_fingerprint,
        &capabilities, &proposals)?;
    let digest = sha256_hex(&serde_json::to_vec(&terms).map_err(|_| "approval encoding failed")?);
    if mode == "prepare" {
        write_new(
            &report,
            &json!({"kind": "release_analysis_plan", "approvalDigest": digest,
            "approval": terms, "note": "Five SAM pages, at most 20 tile POSTs; actual GPU cost is not estimated."}),
        )?;
        return Ok(());
    }
    let approved = std::env::var("MC_RELEASE_ANALYSIS_APPROVE_DIGEST")
        .map_err(|_| "set approved analysis digest")?;
    let saved: Value =
        serde_json::from_slice(&fs::read(&report).map_err(|_| "analysis plan unavailable")?)
            .map_err(|_| "analysis plan unreadable")?;
    if saved["kind"] != "release_analysis_plan"
        || saved["approvalDigest"] != digest
        || saved["approval"] != terms
        || approved != digest
    {
        return Err("analysis plan differs from approved inputs or deployment".into());
    }
    fs::create_dir(&output).map_err(|_| "fresh analysis output directory required")?;
    let gateway = CaptureGateway::new(client, handle.clone(), profile_id.clone(), endpoint_fingerprint);
    let journal = analysis::journal_for(handle).map_err(|_| "analysis journal unavailable")?;
    let mut progress = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("analysis-progress.jsonl"))
        .map_err(|_| "fresh analysis progress required")?;
    for (index, proposal) in proposals.iter().enumerate() {
        append(
            &mut progress,
            &json!({"stage": "confirmed_starting", "page": PAGES[index].0,
            "proposalId": proposal.proposal_id}),
        )?;
        let began = Instant::now();
        let analysis = analysis::confirm_and_run(
            &proposal.proposal_id,
            true,
            true,
            &gateway,
            &journal,
            |_| {},
        )
        .map_err(|_| "production analysis confirmation or run failed")?;
        let value =
            serde_json::to_value(&analysis).map_err(|_| "analysis result encoding failed")?;
        let data_url = value["maskDataUrl"]
            .as_str()
            .ok_or("stitched SAM mask missing")?;
        let encoded = data_url
            .strip_prefix("data:image/png;base64,")
            .ok_or("stitched SAM mask encoding invalid")?;
        let mask_png = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "stitched SAM mask encoding invalid")?;
        write_new_bytes(
            &output.join(format!("{}.stitched.png", PAGES[index].0)),
            &mask_png,
        )?;
        let record = journal
            .read(&proposal.proposal_id)
            .map_err(|_| "analysis journal unavailable")?;
        let captured = gateway
            .records
            .lock()
            .map_err(|_| "analysis capture unavailable")?;
        let page_rows: Vec<_> = captured
            .iter()
            .filter(|row| row.request.source_page_sha256 == PAGES[index].1)
            .collect();
        if page_rows.len() != proposal.tiles.len() {
            return Err("analysis tile capture incomplete".into());
        }
        for (tile_index, row) in page_rows.iter().enumerate() {
            let png = row
                .result
                .mask_bytes()
                .map_err(|_| "analysis tile mask invalid")?
                .ok_or("analysis tile mask missing")?;
            write_new_bytes(
                &output.join(format!("{}.tile-{tile_index}.png", PAGES[index].0)),
                &png,
            )?;
        }
        let rows: Vec<_> = page_rows
            .iter()
            .map(|row| {
                json!({
                    "tileId": row.request.tile_id, "tileRect": row.request.tile_rect,
                    "tileCore": row.request.tile_core, "requestDigest": row.request.request_digest,
                    "elapsedMs": row.elapsed_ms, "timings": row.result.timings,
                    "componentCount": row.result.components.len(), "boxCount": row.result.boxes.len(),
                    "reportedCostUsd": row.result.reported_cost_usd,
                })
            })
            .collect();
        write_new(
            &output.join(format!("{}.json", PAGES[index].0)),
            &json!({
                "sourcePngSha256": PAGES[index].1, "graphSha256s": proposal.graph_sha256s,
                "modelRevision": proposal.model_revision, "analysisProtocol": VERSION,
                "pageWidth": proposal.page_width, "pageHeight": proposal.page_height,
                "tileCount": page_rows.len(), "tiles": rows,
                "elapsedMs": began.elapsed().as_millis(), "reportedCostUsd": record.reported_cost_usd,
                "maskSha256": value["maskSha256"],
                "componentCount": value["evidence"]["components"].as_array().map(Vec::len),
                "groupCount": value["evidence"]["groups"].as_array().map(Vec::len),
                "groupBounds": value["evidence"]["groups"].as_array().map(|groups|
                    groups.iter().map(|group| group["bounds"].clone()).collect::<Vec<_>>()),
            }),
        )?;
        append(
            &mut progress,
            &json!({"stage": "page_complete", "page": PAGES[index].0,
            "tilePosts": page_rows.len(), "totalPosts": gateway.count.load(Ordering::SeqCst)}),
        )?;
    }
    append(
        &mut progress,
        &json!({"stage": "complete", "pages": 5,
        "totalPosts": gateway.count.load(Ordering::SeqCst)}),
    )?;
    Ok(())
}

fn approval_terms(
    chapter_id: &str,
    profile_id: &str,
    endpoint_fingerprint: &str,
    capabilities: &cleaner_core::cloud_analysis_wire::AnalysisCapabilities,
    proposals: &[AnalysisProposal],
) -> Result<Value, String> {
    if proposals.len() != PAGES.len() {
        return Err("expected five analysis proposals".into());
    }
    let mut pages = Vec::new();
    for (index, proposal) in proposals.iter().enumerate() {
        let planned = cleaner_core::cloud_tiles::planned(1136, 1601)
            .map_err(|_| "analysis tile plan unavailable")?;
        if proposal.page_index != index
            || proposal.chapter_id != chapter_id
            || proposal.provider != CloudProvider::Modal
            || proposal.profile_id != profile_id
            || analysis::proposal_endpoint_fingerprint(&proposal.proposal_id)? != endpoint_fingerprint
            || proposal.source_page_sha256 != PAGES[index].1
            || proposal.page_width != 1136
            || proposal.page_height != 1601
            || proposal.tiles.len() != 4
            || proposal.capability != SAM
            || proposal.models.len() != 1
            || proposal.models[0].capability != SAM
            || !proposal
                .tiles
                .iter()
                .zip(&planned)
                .all(|(tile, expected)| tile.rect == expected.tile)
        {
            return Err("analysis proposal differs from five-page SAM plan".into());
        }
        pages.push(
            json!({"page": PAGES[index].0, "pageIndex": proposal.page_index,
            "sourcePngSha256": proposal.source_page_sha256,
            "underlaySha256": proposal.underlay_sha256,
            "predecessorsSha256": proposal.predecessors_sha256,
            "projectRevisionSha256": proposal.project_revision_sha256,
            "graphSha256s": proposal.graph_sha256s, "modelRevision": proposal.model_revision,
            "tiles": proposal.tiles, "totalTilePixels": proposal.total_tile_pixels,
            "totalEncodedBytes": proposal.total_encoded_bytes}),
        );
    }
    Ok(
        json!({"appId": APP_ID, "chapterId": chapter_id, "provider": "modal",
        "profileId": profile_id, "endpointFingerprint": endpoint_fingerprint,
        "analysisProtocol": capabilities.protocol_version,
        "analysisCapabilities": capabilities.capabilities, "analysisLimits": capabilities.limits,
        "maxTilePosts": MAX_POSTS, "pages": pages}),
    )
}

fn write_new(path: &Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| "probe report encoding failed")?;
    write_new_bytes(path, &bytes)
}

fn write_new_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "fresh probe artifact required")?;
    file.write_all(bytes)
        .map_err(|_| "probe artifact write failed")?;
    file.sync_all()
        .map_err(|_| "probe artifact sync failed".into())
}

fn append(file: &mut File, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *file, value).map_err(|_| "probe progress encoding failed")?;
    file.write_all(b"\n")
        .map_err(|_| "probe progress write failed")?;
    file.sync_all()
        .map_err(|_| "probe progress sync failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::cloud_tiles;

    #[test]
    fn five_page_plan_is_exactly_twenty_overlapping_tiles() {
        for _ in PAGES {
            let tiles = cloud_tiles::planned(1136, 1601).unwrap();
            assert_eq!(tiles.len(), 4);
            assert!(tiles
                .iter()
                .all(|tile| tile.tile.width == 1024 && tile.tile.height == 1024));
            assert!(tiles
                .iter()
                .all(|tile| tile.core.width > 0 && tile.core.height > 0));
        }
        assert_eq!(
            PAGES.len() * cloud_tiles::planned(1136, 1601).unwrap().len(),
            MAX_POSTS
        );
    }
}
