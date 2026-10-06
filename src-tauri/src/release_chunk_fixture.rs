//! Debug-only creator for copied, mask-backed cloud clean boundary chapters.
//! It never contacts a gateway or opens a credential store.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use cleaner_core::engines::render::{CloudProvider, Preprocessing, RenderRecipe};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::project::{Job, StripMode};
use serde_json::json;
use tauri::Manager;

use crate::inference::consent::load_cloud_region;
use crate::inference::cloud_clean::{self, CleanGateway, Destination, PrepareRequest};
use crate::library::{Library, NewChapter};

const APP_ID: &str = "com.mangacleaner.validation20260927";
const APP_DATA: &str = "/Users/caved/Library/Application Support/com.mangacleaner.validation20260927";
const BASELINE: &str = "/Users/caved/dev/manga-release-validation-2026-09-27/live/detect-baseline/c2.mtclean";
const OUTPUT: &str = "/Users/caved/dev/manga-release-validation-2026-09-27/live/chunk-boundary-fixture";
const QUALITY_SOURCE: &str = "/Users/caved/dev/manga-release-validation-2026-09-27/evidence/c32.mtclean";
const QUALITY_DETECT: &str = "/Users/caved/dev/manga-release-validation-2026-09-27/evidence/detect-snapshot/c32.mtclean";
const QUALITY_IDS: [&str; 5] = [
    "c32-p006-d189", "c32-p004-d106", "c32-p004-d102", "c32-p006-d190", "c32-p002-d27",
];
const CYCLES: usize = 13;
const BASE_PAGES: usize = 5;
const BASE_REGIONS: usize = 21;

pub fn main() -> Result<(), String> {
    let mode = std::env::var("MC_RELEASE_CHUNK_FIXTURE_MODE")
        .map_err(|_| "set MC_RELEASE_CHUNK_FIXTURE_MODE=prepare or execute")?;
    let baseline = Job::open_read_only(Path::new(BASELINE)).map_err(|e| e.to_string())?;
    verify_baseline(&baseline)?;
    let quality_source = Job::open_read_only(Path::new(QUALITY_SOURCE)).map_err(|e| e.to_string())?;
    let quality_detect = Job::open_read_only(Path::new(QUALITY_DETECT)).map_err(|e| e.to_string())?;
    verify_quality_inputs(&quality_source, &quality_detect)?;
    if mode == "prepare" {
        println!("{}", json!({
            "baseline": BASELINE,
            "baselineSha256": sha256_hex(&fs::read(BASELINE).map_err(|e| e.to_string())?),
            "plannedCopiedPages": BASE_PAGES * CYCLES,
            "plannedStoredDetections": BASE_REGIONS * CYCLES,
            "chunkBoundary": cloud_clean::CHUNK_REGIONS,
            "eligibility": "checked by production prepare after fixture creation",
            "recoveryChapterRegions": 1,
            "qualityChapterRegions": QUALITY_IDS.len(),
            "destination": OUTPUT,
            "mode": "prepare"
        }));
        return Ok(());
    }
    if mode != "execute" || std::env::var("MC_RELEASE_CHUNK_FIXTURE_EXECUTE").as_deref() != Ok("1") {
        return Err("execute requires MC_RELEASE_CHUNK_FIXTURE_EXECUTE=1".into());
    }
    let mut context = crate::app_context();
    if context.config().identifier != APP_ID { return Err("validation app identifier required".into()); }
    context.config_mut().app.windows.clear();
    let app = tauri::Builder::default().build(context)
        .map_err(|_| "validation app could not start")?;
    if app.handle().path().app_data_dir().map_err(|_| "validation app path unavailable")?
        != Path::new(APP_DATA) {
        return Err("validation app data directory required".into());
    }
    let library = Library::for_app(app.handle()).map_err(|e| e.to_string())?;
    let existing = library.list_projects().map_err(|e| e.to_string())?;
    if existing.iter().any(|project| project.name.starts_with("Release chunk boundary")) {
        return Err("chunk boundary fixture already exists in validation library".into());
    }
    let output = Path::new(OUTPUT);
    crate::release_probe::validate_release_artifact_path(output)?;
    if output.exists() { return Err("chunk boundary output already exists".into()); }
    fs::create_dir(output).map_err(|e| e.to_string())?;
    let (chunk_project, chunk_chapter, chunk_path) = build_chapter(&library, &baseline, output,
        "Release chunk boundary 273", "Cloud chunk boundary", CYCLES, None)?;
    let (recovery_project, recovery_chapter, recovery_path) = build_chapter(&library, &baseline, output,
        "Release chunk boundary recovery", "One unknown render", 1, Some(1))?;
    let (quality_project, quality_chapter, quality_path) =
        build_quality_chapter(&library, &quality_source, &quality_detect, output)?;
    let offline = offline_plan(&chunk_path, &chunk_chapter)?;
    if offline.regions != 273 || offline.chunks != 2 || offline.region_ids.len() != 273 {
        return Err("fixture did not cross the production cloud clean chunk boundary".into());
    }
    let chunk_lengths: Vec<usize> = (0..offline.chunks as usize)
        .map(|index| crate::inference::consent::chunk_range(
            offline.region_ids.len(), offline.chunk_regions as usize, index)
            .map(|range| range.len()).ok_or("fixture chunk range missing"))
        .collect::<Result<_, _>>()?;
    if chunk_lengths != [256, 17] { return Err("fixture chunk lengths changed".into()); }
    let report = json!({
        "mode": "execute",
        "baselineSha256": sha256_hex(&fs::read(BASELINE).map_err(|e| e.to_string())?),
        "chunkProjectId": chunk_project, "chunkChapterId": chunk_chapter,
        "chunkManifest": chunk_path, "chunkPages": BASE_PAGES * CYCLES,
        "chunkRegions": offline.regions, "chunkSize": offline.chunk_regions,
        "chunks": offline.chunks, "chunkLengths": chunk_lengths,
        "recoveryProjectId": recovery_project, "recoveryChapterId": recovery_chapter,
        "recoveryManifest": recovery_path, "recoveryRegions": 1,
        "qualityProjectId": quality_project, "qualityChapterId": quality_chapter,
        "qualityManifest": quality_path, "qualityRegionIds": QUALITY_IDS,
        "source": "copied Detect baseline; each region is a stored mask with identical page bytes",
        "gatewayCalls": 0
    });
    let report_path = output.join(format!("{}.{}", "fixture-report", "json"));
    fs::write(&report_path, serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("{}", report);
    Ok(())
}

struct OfflineRecipe;

impl CleanGateway for OfflineRecipe {
    fn recipe(&self) -> Result<RenderRecipe, String> {
        Ok(RenderRecipe::new("release-fixture", "2.0.0", "flux-fixture",
            "0123456789abcdef0123456789abcdef01234567", false))
    }
    fn gpu_price(&self) -> Option<(String, f64)> { None }
}

fn offline_plan(path: &Path, chapter_id: &str) -> Result<cloud_clean::CloudCleanProposal, String> {
    cloud_clean::prepare(path, PrepareRequest {
        qwen_edit: None,
        chapter_id: chapter_id.into(), scope: "chapter".into(), page_indices: None,
        region_ids: None, local_first: false, picks: None,
    }, Destination {
        provider: CloudProvider::Modal, profile_id: "fixture-offline".into(),
        profile_name: "Fixture offline".into(), endpoint_fingerprint: "f".repeat(64),
    }, &OfflineRecipe, &|_, _| false)
}

fn verify_quality_inputs(source_job: &Job, detect_job: &Job) -> Result<(), String> {
    if source_job.project.sources.len() != detect_job.project.sources.len() {
        return Err("quality source and Detect snapshot differ in page count".into());
    }
    for &id in &QUALITY_IDS {
        let row = detect_job.project.detections.iter().find(|row| row.id == id)
            .ok_or("quality Detect row missing")?;
        if source_job.project.sources[row.source_idx].sha256 != detect_job.project.sources[row.source_idx].sha256 {
            return Err("quality source changed since Detect".into());
        }
        let source = source_job.source_path(row.source_idx).ok_or("quality page missing")?;
        if sha256_hex(&fs::read(source).map_err(|e| e.to_string())?)
            != source_job.project.sources[row.source_idx].sha256 {
            return Err("quality page digest changed".into());
        }
        detect_job.load_detection(id).map_err(|e| e.to_string())?
            .ok_or("quality Detect mask missing")?;
        let ink_path = source_job.sidecar().join(format!("{id}.ink"));
        cleaner_core::project::buffers::decode_mask(&fs::read(ink_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn build_quality_chapter(
    library: &Library, source_job: &Job, detect_job: &Job, output: &Path,
) -> Result<(String, String, PathBuf), String> {
    let stage = output.join("quality-sources");
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let selected = [1usize, 3, 5];
    for (position, &index) in selected.iter().enumerate() {
        let source = source_job.source_path(index).ok_or("quality source missing")?;
        fs::copy(source, stage.join(format!("{:03}.png", position + 1))).map_err(|e| e.to_string())?;
    }
    let project = library.create_project("Release chunk boundary quality", StripMode::Single, None, None)
        .map_err(|e| e.to_string())?;
    let chapter = match library.create_chapter(&project.id, "Five exact mask controls", Some(1), Some(stage))
        .map_err(|e| e.to_string())? {
        NewChapter::Created(chapter) => chapter,
        _ => return Err("quality chapter was not created".into()),
    };
    let path = library.resolve_chapter(&chapter.id).map_err(|e| e.to_string())?;
    let mut job = Job::open(&path).map_err(|e| e.to_string())?;
    if job.project.sources.len() != selected.len() { return Err("quality ingest rejected copied source".into()); }
    for (position, &index) in selected.iter().enumerate() {
        if job.project.sources[position].sha256 != source_job.project.sources[index].sha256 {
            return Err("quality source changed during import".into());
        }
    }
    for &id in &QUALITY_IDS {
        let loaded = detect_job.load_detection(id).map_err(|e| e.to_string())?
            .ok_or("quality Detect mask missing")?;
        let mut record = loaded.record;
        record.source_idx = selected.iter().position(|source| *source == record.source_idx)
            .ok_or("quality source index missing")?;
        let ink_path = source_job.sidecar().join(format!("{id}.ink"));
        let ink = cleaner_core::project::buffers::decode_mask(
            &fs::read(ink_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        job.store_detection(record, &loaded.mask, &ink).map_err(|e| e.to_string())?;
    }
    job.project.counters.detections = QUALITY_IDS.len() as u32;
    job.flush().map_err(|e| e.to_string())?;
    validate_chapter(&path, QUALITY_IDS.len())?;
    let saved = Job::open_read_only(&path).map_err(|e| e.to_string())?;
    if !saved.project.patches.is_empty()
        || QUALITY_IDS.iter().any(|id| !saved.project.detections.iter().any(|row| row.id == *id)) {
        return Err("quality chapter lost exact detection identities".into());
    }
    Ok((project.id, chapter.id, path))
}

fn verify_baseline(job: &Job) -> Result<(), String> {
    if job.project.strip.mode != StripMode::Single
        || job.project.sources.len() != BASE_PAGES
        || job.project.strip.order != (0..BASE_PAGES).collect::<Vec<_>>()
        || job.project.detections.len() != BASE_REGIONS
        || !job.project.patches.is_empty() {
        return Err("copied Detect baseline shape changed".into());
    }
    for (index, source) in job.project.sources.iter().enumerate() {
        let path = job.source_path(index).ok_or("baseline source missing")?;
        if sha256_hex(&fs::read(path).map_err(|e| e.to_string())?) != source.sha256 {
            return Err("baseline source digest changed".into());
        }
    }
    let mut ids = HashSet::new();
    for row in &job.project.detections {
        if !ids.insert(&row.id) || row.source_idx >= BASE_PAGES { return Err("baseline detection identity changed".into()); }
        job.load_detection(&row.id).map_err(|e| e.to_string())?
            .ok_or("baseline detection missing")?;
        if let Some(group) = &row.group {
            if let (Some(reference), Some(expected)) = (&group.evidence_ref, &group.evidence_sha256) {
                let path = job.sidecar().join(reference);
                if sha256_hex(&fs::read(path).map_err(|e| e.to_string())?) != *expected {
                    return Err("baseline evidence digest changed".into());
                }
            }
        }
    }
    Ok(())
}

fn build_chapter(
    library: &Library, baseline: &Job, output: &Path,
    project_name: &str, chapter_name: &str, cycles: usize, only_source: Option<usize>,
) -> Result<(String, String, PathBuf), String> {
    let stage = output.join(if only_source.is_some() { "recovery-sources" } else { "chunk-sources" });
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let selected: Vec<usize> = only_source.map_or_else(|| (0..BASE_PAGES).collect(), |index| vec![index]);
    for (position, &source_idx) in selected.iter().enumerate() {
        let source = baseline.source_path(source_idx).ok_or("baseline source missing")?;
        fs::copy(source, stage.join(format!("{:03}.png", position + 1))).map_err(|e| e.to_string())?;
    }
    let project = library.create_project(project_name, StripMode::Single, None, None)
        .map_err(|e| e.to_string())?;
    let chapter = match library.create_chapter(&project.id, chapter_name, Some(1), Some(stage))
        .map_err(|e| e.to_string())? {
        NewChapter::Created(chapter) => chapter,
        _ => return Err("validation fixture chapter was not created".into()),
    };
    let path = library.resolve_chapter(&chapter.id).map_err(|e| e.to_string())?;
    let mut job = Job::open(&path).map_err(|e| e.to_string())?;
    if job.project.sources.len() != selected.len() { return Err("fixture ingest rejected copied source".into()); }
    for (position, &source_idx) in selected.iter().enumerate() {
        if job.project.sources[position].sha256 != baseline.project.sources[source_idx].sha256 {
            return Err("fixture source bytes changed during ingest".into());
        }
    }
    let ingested = job.project.sources.clone();
    job.project.sources = (0..cycles).flat_map(|_| ingested.iter().cloned()).collect();
    job.project.strip.order = (0..job.project.sources.len()).collect();
    job.flush().map_err(|e| e.to_string())?;
    fs::create_dir_all(job.sidecar().join("evidence")).map_err(|e| e.to_string())?;
    for row in &baseline.project.detections {
        let Some(group) = &row.group else { continue };
        let Some(reference) = &group.evidence_ref else { continue };
        let destination = job.sidecar().join(reference);
        if !destination.exists() {
            fs::copy(baseline.sidecar().join(reference), &destination).map_err(|e| e.to_string())?;
        }
    }
    let original_rows: Vec<_> = baseline.project.detections.iter()
        .filter(|row| only_source.is_none_or(|source| row.source_idx == source))
        .collect();
    if only_source.is_some() && original_rows.len() != 1 {
        return Err("recovery source no longer has one detection".into());
    }
    for cycle in 0..cycles {
        for original in &original_rows {
            let loaded = baseline.load_detection(&original.id).map_err(|e| e.to_string())?
                .ok_or("baseline detection missing")?;
            let position = selected.iter().position(|source| *source == original.source_idx)
                .ok_or("baseline detection source missing")?;
            let page = cycle * selected.len() + position;
            let page_id = format!("{}-p{:03}", chapter.id, page + 1);
            let mut record = loaded.record;
            record.id = job.try_next_detection_id(&page_id).map_err(|e| e.to_string())?;
            record.source_idx = page;
            job.store_detection(record, &loaded.mask, &loaded.ink).map_err(|e| e.to_string())?;
        }
    }
    validate_chapter(&path, cycles * original_rows.len())?;
    Ok((project.id, chapter.id, path))
}

fn validate_chapter(path: &Path, expected: usize) -> Result<(), String> {
    let job = Job::open_read_only(path).map_err(|e| e.to_string())?;
    if job.project.detections.len() != expected { return Err("fixture detection count changed".into()); }
    let mut ids = HashSet::new();
    for row in &job.project.detections {
        if !ids.insert(&row.id) { return Err("fixture detection id repeated".into()); }
        let source = job.source_path(row.source_idx).ok_or("fixture source missing")?;
        let source_hash = sha256_hex(&fs::read(source).map_err(|e| e.to_string())?);
        if source_hash != job.project.sources[row.source_idx].sha256 {
            return Err("fixture source digest changed".into());
        }
        let region = load_cloud_region(&job, &row.id, &source_hash).map_err(|e| e.to_string())?;
        let page = &job.project.sources[row.source_idx];
        if region.sendable_crop(Preprocessing::V2, page.w, page.h).is_none() {
            return Err("fixture region is outside current cloud render limits".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires copied release validation evidence"]
    fn copied_detect_fixture_has_real_regions_across_chunk_boundary() {
        let baseline = Job::open_read_only(Path::new(BASELINE)).unwrap();
        verify_baseline(&baseline).unwrap();
        let quality_source = Job::open_read_only(Path::new(QUALITY_SOURCE)).unwrap();
        let quality_detect = Job::open_read_only(Path::new(QUALITY_DETECT)).unwrap();
        verify_quality_inputs(&quality_source, &quality_detect).unwrap();
        let root = std::env::temp_dir().join(format!("mc-chunk-fixture-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&root).unwrap();
        let library = Library::at(root.join("library"));
        let (_, _, chunk_path) = build_chapter(&library, &baseline, &root,
            "Release chunk boundary test", "Cloud chunk boundary", CYCLES, None).unwrap();
        let (_, _, recovery_path) = build_chapter(&library, &baseline, &root,
            "Release recovery test", "One unknown render", 1, Some(1)).unwrap();
        let (_, _, quality_path) = build_quality_chapter(&library, &quality_source, &quality_detect, &root).unwrap();
        let chunk = Job::open_read_only(&chunk_path).unwrap();
        assert_eq!(chunk.project.strip.order.len(), 65);
        assert_eq!(chunk.project.detections.len(), 273);
        assert_eq!(chunk.project.counters.detections, 273);
        let proposal = offline_plan(&chunk_path, &chunk_path.file_stem().unwrap().to_string_lossy()).unwrap();
        assert_eq!(proposal.regions, 273);
        assert_eq!(proposal.chunk_regions, cloud_clean::CHUNK_REGIONS as u32);
        assert_eq!(proposal.chunks, 2);
        assert_eq!(proposal.region_ids.len(), 273);
        let recovery = Job::open_read_only(&recovery_path).unwrap();
        assert_eq!(recovery.project.detections.len(), 1);
        let quality = Job::open_read_only(&quality_path).unwrap();
        assert_eq!(quality.project.detections.len(), 5);
        assert!(quality.project.patches.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
