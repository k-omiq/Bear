//! One-shot, debug-only GET probe for a journaled ambiguous submission.

use std::{fs, path::PathBuf};

use cleaner_core::{
    cloud_wire::{JobExecutionStatus, ResultMetadata},
    engines::render::CloudProvider,
};
use sha2::{Digest, Sha256};
use tauri::Manager;

use crate::inference::{commands, config, journal::AttemptPhase};

const ATTEMPTS: &[(&str, &str, &str)] = &[
    ("att-4e93cd273466fde8d888d0f2", "job-9351880d3753c524b98dc152", "c32-p003-d61"),
    ("att-18fc9a616809c52b51492dda", "job-d0e54b5465e5207d97a5580c", "c32-p005-d125"),
    ("att-5952cd9a5210a4e6c3ba9772", "job-6bae89508e12bf12b9120fde", "c32-p005-d167"),
];
const ROOT: &str = "/Users/caved/dev/demo-cloud-results-2026-09-27";

pub fn main() -> Result<(), String> {
    let diagnostic = std::env::var("MC_CLOUD_ATTEMPT_DIAG").as_deref() == Ok("1");
    if !diagnostic && std::env::var("MC_CLOUD_ATTEMPT_PROBE").as_deref() != Ok("1") {
        return Err("set MC_CLOUD_ATTEMPT_PROBE=1 for GET or MC_CLOUD_ATTEMPT_DIAG=1 for local input comparison".into());
    }
    let mut context = crate::app_context();
    context.config_mut().app.windows.clear();
    let app = tauri::Builder::default().build(context).map_err(|e| format!("Tauri app: {e}"))?;
    let handle = app.handle();
    let expected = PathBuf::from("/Users/caved/Library/Application Support/com.mangacleaner.studio");
    if handle.path().app_data_dir().map_err(|e| e.to_string())? != expected {
        return Err("unexpected application data directory".into());
    }

    if diagnostic {
        let path = crate::library::Library::for_app(handle).map_err(|e| e.to_string())?
            .resolve_chapter("c32").map_err(|e| e.to_string())?;
        let journal = commands::get_app_journal(handle)?;
        for attempt_id in ["att-42741f59a54162b97ff2503f", "att-2dc32f68e4a67d333260ad25"] {
            let record = journal.get_record(attempt_id).map_err(|e| e.to_string())?;
            let recipe = cleaner_core::engines::render::RenderRecipe::new(
                &record.recipe_id, &record.preprocessing_version, &record.model_id,
                &record.model_revision, record.native_mask_conditioning,
            );
            let input = crate::inference::service::read_render_input(
                &path, record.page_index.ok_or("missing page index")?, &record.region_id, &recipe,
            ).map_err(|e| e.to_string())?;
            println!("{}", serde_json::json!({
                "attemptId": attempt_id, "regionId": record.region_id,
                "sourceMatches": input.source_hash == record.source_image_hash,
                "revisionMatches": input.revision == record.region_revision,
                "inputMatches": Some(input.input_sha256.as_str()) == record.input_sha256.as_deref(),
                "predecessorsMatch": Some(input.predecessors_sha256.as_str()) == record.predecessors_sha256.as_deref(),
                "cropMatches": input.crop_sha256 == record.crop_sha256,
                "hintMatches": input.hint_sha256 == record.hint_sha256,
            }));
        }
        return Ok(());
    }

    let service = commands::app_inference_service(handle)?;
    let config = config::read_inference_config(handle)?;
    let only = std::env::var("MC_CLOUD_ATTEMPT_PROBE_ID").ok();
    for &(attempt_id, job_id, region_id) in ATTEMPTS {
        if only.as_deref().is_some_and(|id| id != attempt_id) { continue; }
        let record = service.journal().get_record(attempt_id).map_err(|e| e.to_string())?;
        if record.job_id != job_id || record.provider != CloudProvider::Modal || record.region_id != region_id
            || !matches!(record.phase, AttemptPhase::Unknown { .. } | AttemptPhase::Accepted { .. })
        {
            return Err(format!("journal identity or phase differs for {attempt_id}"));
        }
        let profile = config.modal_profiles.get(&record.profile_id).ok_or("journal profile is missing")?;
        let endpoint_fingerprint = config::compute_canonical_endpoint_fingerprint(&profile.endpoint_url)?;
        if record.endpoint_fingerprint != endpoint_fingerprint {
            return Err(format!("journal endpoint fingerprint differs for {attempt_id}"));
        }
        let hash = Sha256::digest(format!("{}\0{}", record.job_id, record.attempt_id).as_bytes());
        let handle_id = format!("handle-modal-{}", hash[..16].iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        if let AttemptPhase::Accepted { handle } = &record.phase {
            if handle != &handle_id { return Err(format!("journal handle differs for {attempt_id}")); }
        }
        let client = service.build_http_client(record.provider, &record.profile_id, &config)
            .map_err(|e| e.to_string())?;
        let request = record.to_request_metadata();
        let status = client.get_job_status(&handle_id, &request).map_err(|e| e.to_string())?;

        let mut saved_result = None;
        if status.status == JobExecutionStatus::Completed {
            let result_meta = ResultMetadata {
                handle: handle_id.clone(), job_id: request.job_id.clone(), attempt_id: request.attempt_id.clone(),
                request_digest: request.request_digest.clone(), recipe_id: request.recipe.recipe_id.clone(),
                preprocessing_version: request.recipe.preprocessing_version.clone(),
                model_id: request.recipe.model_id.clone(), model_revision: request.recipe.model_revision.clone(),
                native_mask_conditioning: request.recipe.native_mask_conditioning,
                result_digest: status.result_digest.clone().ok_or("completed status lacks result digest")?,
                reported_cost_usd: status.reported_cost_usd, width: request.width, height: request.height,
                byte_length: status.result_bytes.ok_or("completed status lacks result byte length")?,
            };
            let png = client.fetch_result_bytes(&handle_id, &result_meta, &request, service.journal().limits())
                .map_err(|e| e.to_string())?;
            let path = PathBuf::from(ROOT).join(format!("cloud-attempt-probe-{attempt_id}-result.png"));
            fs::write(&path, png).map_err(|e| e.to_string())?;
            saved_result = Some(path.display().to_string());
        }
        let output = serde_json::json!({
            "attemptId": attempt_id, "jobId": job_id, "regionId": record.region_id,
            "handle": handle_id, "status": status.status,
            "resultDigest": status.result_digest, "resultBytes": status.result_bytes,
            "reportedCostUsd": status.reported_cost_usd,
            "errorCode": status.error.as_ref().map(|e| e.error_code.as_str()),
            "savedResult": saved_result,
        });
        let output_path = PathBuf::from(ROOT).join(format!("cloud-attempt-probe-{attempt_id}.json"));
        fs::write(&output_path, serde_json::to_vec_pretty(&output).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        println!("{}", output);
        println!("probe saved to {}", output_path.display());
    }
    Ok(())
}
