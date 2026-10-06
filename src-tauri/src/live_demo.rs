//! Opt-in live integration exercise, built only in debug mode.
//! `MC_LIVE_DEMO_PREFLIGHT=1 cargo run -p manga-cleaner --example live_demo`
//! `MC_LIVE_DEMO=1 cargo run -p manga-cleaner --example live_demo`

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use cleaner_core::{
    engines::render::ExecutionTarget,
    project::{Job, StripMode},
};
use tauri::Manager;

use crate::{
    events, exporting,
    inference::{cloud_clean, config, run_analysis},
    library::Library,
    run,
};

const ROOT: &str = "/Users/caved/dev/demo-cloud-results-2026-09-27";

pub fn main() -> Result<(), String> {
    let preflight = std::env::var("MC_LIVE_DEMO_PREFLIGHT").as_deref() == Ok("1");
    let live = std::env::var("MC_LIVE_DEMO").as_deref() == Ok("1");
    if preflight == live {
        return Err("set exactly one of MC_LIVE_DEMO_PREFLIGHT=1 or MC_LIVE_DEMO=1".into());
    }
    tauri::async_runtime::block_on(exercise(preflight))
}

async fn exercise(preflight: bool) -> Result<(), String> {
    let mut context = crate::app_context();
    context.config_mut().app.windows.clear();
    let app = tauri::Builder::default()
        .build(context)
        .map_err(|e| format!("Tauri app: {e}"))?;
    let handle = app.handle().clone();
    let actual = handle.path().app_data_dir().map_err(|e| e.to_string())?;
    let expected =
        PathBuf::from("/Users/caved/Library/Application Support/com.mangacleaner.studio");
    if actual != expected {
        return Err(format!(
            "unexpected app data directory: {}",
            actual.display()
        ));
    }
    if preflight {
        println!(
            "headless Tauri context ready; app data: {}",
            actual.display()
        );
        return Ok(());
    }
    if !crate::inference::cloud_allowed(&handle) {
        return Err("cloud is disabled in saved settings".into());
    }
    let config = config::read_inference_config(&handle)?;
    let (provider, profile_id) = match config.selected_target {
        ExecutionTarget::Modal { profile_id } => (
            cleaner_core::engines::render::CloudProvider::Modal,
            profile_id,
        ),
        ExecutionTarget::Beam { profile_id } => (
            cleaner_core::engines::render::CloudProvider::Beam,
            profile_id,
        ),
        ExecutionTarget::Local => return Err("select a cloud profile before running".into()),
    };
    let root = PathBuf::from(ROOT);
    let resume_empty = std::env::var("MC_LIVE_DEMO_RESUME_EMPTY").as_deref() == Ok("1");
    let resume = std::env::var("MC_LIVE_DEMO_RESUME").as_deref() == Ok("1");
    if resume_empty && resume {
        return Err("choose one live demo recovery mode".into());
    }
    let source_first = root.join("source-detect-then-clean");
    let source_second = root.join("source-auto-clean");
    for source in [&source_first, &source_second] {
        let count = fs::read_dir(source)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
            .count();
        if count != 25 {
            return Err(format!(
                "expected 25 source PNGs at {}, got {count}",
                source.display()
            ));
        }
    }
    let state_path = root.join("live-demo-state.json");
    let saved: Option<serde_json::Value> = if state_path.exists() {
        if !resume_empty && !resume {
            return Err("live demo state exists; explicit recovery mode required".into());
        }
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let stage = value["stage"].as_str().ok_or("saved stage is missing")?;
        if (resume_empty && stage != "detect-first-starting")
            || (resume && !matches!(stage, "clean-first-starting" | "auto-clean-starting"))
        {
            return Err(format!("recovery mode does not support stage {stage}"));
        }
        Some(value)
    } else {
        if resume_empty || resume { return Err("recovery requested without saved state".into()); }
        None
    };
    let saved_stage = saved.as_ref().and_then(|value| value["stage"].as_str()).map(str::to_owned);
    if !resume
        && (root.join("detect-snapshot").exists()
            || root.join("detect-then-clean-png").exists()
            || root.join("fresh-auto-png").exists())
    {
        return Err("result artifacts already exist; refusing to overwrite".into());
    }
    let events_path = root.join("live-demo-events.jsonl");
    if resume_empty && events_path.exists() {
        fs::rename(
            &events_path,
            root.join("live-demo-events-before-deadlock-fix.jsonl"),
        )
        .map_err(|e| e.to_string())?;
    }
    let events_path = if resume {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
        root.join(format!("live-demo-events-resume-{nonce}.jsonl"))
    } else { events_path };
    let event_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&events_path)
        .map_err(|e| e.to_string())?;
    let observed = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let observed_sink = Arc::clone(&observed);
    let event_file = Arc::new(Mutex::new(event_file));
    let _sink_id = events::register(Box::new(move |event| {
        if let Ok(value) = serde_json::to_value(event) {
            if let Ok(mut file) = event_file.lock() {
                let _ = serde_json::to_writer(&mut *file, &value);
                let _ = file.write_all(b"\n");
                let _ = file.flush();
            }
            if let Ok(mut rows) = observed_sink.lock() {
                rows.push(value);
            }
        }
    }));
    let library = Library::for_app(&handle).map_err(|e| e.to_string())?;
    let (project_id, first_id, second_id) = if let Some(saved) = saved {
        let get = |key: &str| {
            saved[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("missing {key}"))
        };
        let ids = (
            get("projectId")?,
            get("detectChapterId")?,
            get("autoChapterId")?,
        );
        let first = Job::open(&library.job_path(&ids.0, &ids.1)).map_err(|e| e.to_string())?;
        let second = Job::open(&library.job_path(&ids.0, &ids.2)).map_err(|e| e.to_string())?;
        if first.project.sources.len() != 25 || second.project.sources.len() != 25 {
            return Err("recovery refused: expected 25 pages in each chapter".into());
        }
        match saved_stage.as_deref() {
            Some("detect-first-starting") if resume_empty => {
                if !first.project.detections.is_empty() || !first.project.patches.is_empty()
                    || !second.project.detections.is_empty() || !second.project.patches.is_empty() {
                    return Err("empty-run recovery refused: chapter already has results".into());
                }
            }
            Some("clean-first-starting") if resume => {
                if !root.join("detect-snapshot").exists()
                    || first.project.detections.is_empty() || !first.project.patches.is_empty()
                    || first.project.detections.iter().any(|d| d.detector != "cloud")
                    || !second.project.detections.is_empty() || !second.project.patches.is_empty() {
                    return Err("clean-first recovery refused: chapter state does not match saved stage".into());
                }
            }
            Some("auto-clean-starting") if resume => {
                if !root.join("detect-then-clean-png").exists()
                    || !first.project.detections.is_empty() || first.project.patches.is_empty()
                    || first.project.patches.iter().any(|p| p.provenance.execution_provider != "cloud" || p.provenance.cloud.is_none())
                    || second.project.detections.is_empty()
                    || second.project.detections.iter().any(|d| d.detector != "cloud")
                    || second.project.patches.iter().any(|p| p.provenance.execution_provider != "cloud" || p.provenance.cloud.is_none()) {
                    return Err("auto-clean recovery refused: chapter state does not match saved stage".into());
                }
            }
            _ => return Err("unsupported saved stage".into()),
        }
        ids
    } else {
        let project = library
            .create_project(
                "Demo cloud live verification",
                StripMode::Single,
                Some(source_first.clone()),
                None,
            )
            .map_err(|e| e.to_string())?;
        let first = library
            .create_chapter(
                &project.id,
                "Detect then cloud clean",
                Some(1),
                Some(source_first),
            )
            .map_err(|e| e.to_string())?
            .created()
            .ok_or("first chapter was not created")?;
        let second = library
            .create_chapter(
                &project.id,
                "Fresh cloud text cleanup",
                Some(2),
                Some(source_second),
            )
            .map_err(|e| e.to_string())?
            .created()
            .ok_or("second chapter was not created")?;
        (project.id, first.id, second.id)
    };
    let first_path = library.job_path(&project_id, &first_id);
    let second_path = library.job_path(&project_id, &second_id);
    if saved_stage.is_none() || resume_empty {
        write_state(&state_path, &project_id, &first_id, &second_id, "created")?;
    }
    println!(
        "project={} detect_chapter={} auto_chapter={}",
        project_id, first_id, second_id
    );

    if saved_stage.as_deref() != Some("auto-clean-starting") {
        if saved_stage.as_deref() != Some("clean-first-starting") {
            write_state(&state_path, &project_id, &first_id, &second_id, "detect-first-starting")?;
            detect(&handle, &first_id, provider, &profile_id, &observed).await?;
            assert_detected(&first_path)?;
            copy_snapshot(&first_path, &root.join("detect-snapshot"))?;
        }
        write_state(&state_path, &project_id, &first_id, &second_id, "clean-first-starting")?;
        cloud_clean_all(&handle, &first_id, &observed).await?;
        assert_cloud_cleaned(&first_path)?;
        export(&handle, &first_id, &root.join("detect-then-clean-png")).await?;
    }

    // The interface's cloud Auto clean is these same two production halves in order.
    if saved_stage.as_deref() != Some("auto-clean-starting") {
        write_state(&state_path, &project_id, &first_id, &second_id, "auto-detect-starting")?;
        detect(&handle, &second_id, provider, &profile_id, &observed).await?;
        assert_detected(&second_path)?;
    }
    write_state(
        &state_path,
        &project_id,
        &first_id,
        &second_id,
        "auto-clean-starting",
    )?;
    cloud_clean_all(&handle, &second_id, &observed).await?;
    assert_cloud_cleaned(&second_path)?;
    export(&handle, &second_id, &root.join("fresh-auto-png")).await?;
    write_state(&state_path, &project_id, &first_id, &second_id, "completed")?;
    println!("completed; outputs at {}", root.display());
    Ok(())
}

fn write_state(
    path: &Path,
    project: &str,
    first: &str,
    second: &str,
    stage: &str,
) -> Result<(), String> {
    let value = serde_json::json!({"projectId":project,"detectChapterId":first,"autoChapterId":second,"stage":stage});
    fs::write(
        path,
        serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

async fn detect(
    app: &tauri::AppHandle,
    chapter_id: &str,
    provider: cleaner_core::engines::render::CloudProvider,
    profile_id: &str,
    observed: &Arc<Mutex<Vec<serde_json::Value>>>,
) -> Result<(), String> {
    let targets = run_analysis::AnalysisTargets {
        rt_full: true,
        sam: true,
    };
    let proposal = run_analysis::propose_run_analysis(
        app.clone(),
        chapter_id.into(),
        Some("chapter".into()),
        None,
        targets.capabilities(true, true),
        provider,
        profile_id.into(),
    )
    .await?;
    println!(
        "detect proposal: chapter={chapter_id} pages={} tiles={} source_bytes={}",
        proposal.pages, proposal.total_tiles, proposal.source_bytes
    );
    save_proposal(
        chapter_id,
        "detect",
        serde_json::json!({
            "proposalId": proposal.proposal_id, "chapterId": chapter_id,
            "pages": proposal.pages, "tiles": proposal.total_tiles,
            "sourceBytes": proposal.source_bytes, "capabilities": proposal.capabilities,
        }),
    )?;
    let grant = run_analysis::confirm_run_analysis(app.clone(), proposal.proposal_id, true, true)?;
    let run = run::run_clean(
        app.clone(),
        Some("chapter".into()),
        chapter_id.into(),
        None,
        Some("lama".into()),
        Some("fill".into()),
        Some("lama".into()),
        Some("clean".into()),
        Some("#ffffff".into()),
        None,
        None,
        Some(serde_json::json!(["rtFull", "samTs"])),
        Some("legacy".into()),
        Some("all_text".into()),
        Some(false),
        Some(targets.to_value()),
        Some(grant.grant_id),
        Some("detect".into()),
        None,
    )
    .await?;
    wait_run(&run, observed)?;
    Ok(())
}

async fn cloud_clean_all(
    app: &tauri::AppHandle,
    chapter_id: &str,
    observed: &Arc<Mutex<Vec<serde_json::Value>>>,
) -> Result<(), String> {
    let path = Library::for_app(app).map_err(|e| e.to_string())?
        .resolve_chapter(chapter_id).map_err(|e| e.to_string())?;
    let job = Job::open(&path).map_err(|e| e.to_string())?;
    let mut ordered = job.project.detections.iter().map(|d| {
        let page = job.project.strip.order.iter().position(|source| *source == d.source_idx)
            .ok_or_else(|| format!("detection {} has no chapter page", d.id))?;
        Ok((page, d.order, d.id.clone()))
    }).collect::<Result<Vec<_>, String>>()?;
    ordered.sort();
    if ordered.is_empty() { return Err("no detections left for cloud clean".into()); }
    let ids: Vec<String> = ordered.into_iter().map(|(_, _, id)| id).collect();
    for (index, batch) in ids.chunks(512).enumerate() {
        cloud_clean_batch(app, chapter_id, &path, batch, index, observed).await?;
    }
    Ok(())
}

async fn cloud_clean_batch(
    app: &tauri::AppHandle,
    chapter_id: &str,
    path: &Path,
    batch: &[String],
    index: usize,
    observed: &Arc<Mutex<Vec<serde_json::Value>>>,
) -> Result<(), String> {
    let proposal = cloud_clean::prepare_cloud_clean(
        app.clone(),
        chapter_id.into(),
        "regions".into(),
        None,
        Some(batch.to_vec()),
        Some(false),
        None,
        None,
        None,
    )
    .await?;
    if proposal.local_cleaned != 0 || proposal.regions as usize != batch.len() {
        return Err(format!(
            "expected {} cloud-only regions; local={} remote={}",
            batch.len(), proposal.local_cleaned, proposal.regions
        ));
    }
    println!(
        "clean proposal: chapter={chapter_id} batch={index} regions={} crop_pixels={}",
        proposal.regions, proposal.total_crop_pixels,
    );
    let proposal_id = proposal.proposal_id.clone().ok_or("missing clean proposal")?;
    save_proposal(
        chapter_id,
        &format!("clean-{proposal_id}"),
        serde_json::json!({
            "proposalId": proposal_id, "chapterId": chapter_id, "batch": index,
            "regions": proposal.regions, "localCleaned": proposal.local_cleaned,
            "cropPixels": proposal.total_crop_pixels, "regionIds": proposal.region_ids,
        }),
    )?;
    let plan_digest = proposal.plan_digest.clone().ok_or("missing clean plan digest")?;
    let grant = cloud_clean::confirm_cloud_clean(
        app.clone(),
        proposal_id,
        plan_digest,
        true,
        true,
    )?;
    let run = cloud_clean::start_cloud_clean(app.clone(), grant.grant_id).await?;
    wait_run(&run, observed)?;
    let job = Job::open(path).map_err(|e| e.to_string())?;
    for id in batch {
        if job.project.detections.iter().any(|d| &d.id == id)
            || !job.project.patches.iter().any(|p| &p.id == id
                && p.provenance.execution_provider == "cloud" && p.provenance.cloud.is_some()) {
            return Err(format!("batch {index} did not cloud-clean detection {id}"));
        }
    }
    Ok(())
}

fn save_proposal(chapter_id: &str, stage: &str, value: serde_json::Value) -> Result<(), String> {
    let path = PathBuf::from(ROOT).join(format!("proposal-{chapter_id}-{stage}.json"));
    let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    fs::write(path, bytes).map_err(|e| e.to_string())
}

fn wait_run(
    handle: &run::RunHandle,
    observed: &Arc<Mutex<Vec<serde_json::Value>>>,
) -> Result<(), String> {
    let id = handle.run_id.as_deref().ok_or("run did not start")?;
    if handle.already_running == Some(true) {
        return Err("another run is active".into());
    }
    let deadline = Instant::now() + Duration::from_secs(7200);
    while run::is_running(id) {
        if Instant::now() >= deadline {
            return Err(format!("run {id} timed out"));
        }
        thread::sleep(Duration::from_secs(1));
    }
    let rows = observed.lock().map_err(|e| e.to_string())?;
    let finish = rows
        .iter()
        .find(|row| row["type"] == "run-finished" && row["runId"] == id)
        .ok_or_else(|| format!("run {id} had no completion event"))?;
    if finish["reason"] != "completed" {
        return Err(format!("run {id} ended: {}", finish["reason"]));
    }
    let completed = rows
        .iter()
        .filter(|row| row["type"] == "page-done" && row["runId"] == id)
        .count();
    if completed != handle.pages.len()
        || rows
            .iter()
            .any(|row| row["type"] == "notice" && row["key"] == "notice.run.pagesFailed")
    {
        return Err(format!(
            "run {id} incomplete: {completed}/{} page completions",
            handle.pages.len()
        ));
    }
    println!("run {id} finished");
    Ok(())
}

fn assert_detected(path: &Path) -> Result<(), String> {
    let job = Job::open(path).map_err(|e| e.to_string())?;
    let count = job.project.detections.len();
    if count == 0
        || job.project.detections.iter().any(|d| d.detector != "cloud")
        || !job.project.patches.is_empty()
    {
        return Err(format!(
            "detect verification failed: {count} detections, {} patches",
            job.project.patches.len()
        ));
    }
    println!("{}: {count} cloud detections", path.display());
    Ok(())
}

fn assert_cloud_cleaned(path: &Path) -> Result<(), String> {
    let job = Job::open(path).map_err(|e| e.to_string())?;
    let patches = &job.project.patches;
    if patches.is_empty()
        || !job.project.detections.is_empty()
        || patches
            .iter()
            .any(|p| p.provenance.execution_provider != "cloud" || p.provenance.cloud.is_none())
    {
        return Err(format!(
            "clean verification failed: {} patches, {} detections",
            patches.len(),
            job.project.detections.len()
        ));
    }
    println!("{}: {} cloud patches", path.display(), patches.len());
    Ok(())
}

fn copy_snapshot(manifest: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir(destination).map_err(|e| e.to_string())?;
    let name = manifest.file_name().ok_or("manifest has no file name")?;
    fs::copy(manifest, destination.join(name)).map_err(|e| e.to_string())?;
    let sidecar = PathBuf::from(format!("{}.d", manifest.display()));
    copy_tree(
        &sidecar,
        &destination.join(format!("{}.d", name.to_string_lossy())),
    )
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir(destination).map_err(|e| e.to_string())?;
    for item in fs::read_dir(source).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let target = destination.join(item.file_name());
        if item.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_tree(&item.path(), &target)?;
        } else {
            fs::copy(item.path(), target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

async fn export(
    app: &tauri::AppHandle,
    chapter_id: &str,
    destination: &Path,
) -> Result<(), String> {
    let result = exporting::export_chapter(
        app.clone(),
        chapter_id.into(),
        Some("PNG".into()),
        Some(destination.to_string_lossy().into_owned()),
        Some("flattened".into()),
        Some("per-page".into()),
        None,
    )
    .await?;
    if result.status != "exported" {
        return Err(format!("export refused: {:?}", result.reason_key));
    }
    println!(
        "export: {} files to {}",
        result.file_count.unwrap_or(0),
        destination.display()
    );
    Ok(())
}
