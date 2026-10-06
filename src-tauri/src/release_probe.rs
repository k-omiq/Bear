//! Debug-only, one-job recovery probe for an isolated validation app.
//!
//! Prepare writes a reviewable plan and exits. Execute requires its exact
//! digest, rechecks the production consent scope, and makes one render POST.
//! Resume performs recovery only. Nothing here targets the normal app library.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use cleaner_core::cloud_wire::{
    compute_request_digest, JobRequestMetadata, WireRenderRecipe, PROTOCOL_VERSION,
};
use cleaner_core::engines::flux::wire_sampling;
use cleaner_core::engines::render::{CloudProvider, ExecutionTarget, RenderRecipe};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::project::Job;
use serde_json::{json, Value};
use tauri::Manager;

use crate::inference::commands::app_inference_service;
use crate::inference::config::{
    compute_canonical_endpoint_fingerprint, read_inference_config, InferenceConfig,
};
use crate::inference::consent::{
    compute_operation_digest, ConfirmProposalOptions, OperationIntent, PrepareProposalRequest,
};
use crate::inference::journal::{AttemptPhase, CreateAttemptIntent, RecoveryDecision};
use crate::inference::policy::GrantService;
use crate::inference::service::{
    attempt_id_for_nonce, read_render_input, InferenceService, PollOptions,
};
use crate::library::Library;

const APP_ID: &str = "com.mangacleaner.validation20260927";
const APP_DATA: &str =
    "/Users/caved/Library/Application Support/com.mangacleaner.validation20260927";
const ARTIFACT_ROOT: &str = "/Users/caved/dev/manga-release-validation-2026-09-27";
const MAX_RECOVERY: Duration = Duration::from_secs(15 * 60);

/// Resolve the existing prefix so a symlink cannot move a probe artifact out
/// of the validation evidence tree, including when the final file is new.
pub(crate) fn validate_release_artifact_path(path: &Path) -> Result<(), String> {
    validate_artifact_path_with_root(path, Path::new(ARTIFACT_ROOT))
}

fn validate_artifact_path_with_root(path: &Path, root: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("probe artifact path must be absolute without parent traversal".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "validation artifact root unavailable")?;
    let mut existing = path;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or("probe artifact path has no existing parent")?;
    }
    let existing = existing
        .canonicalize()
        .map_err(|_| "probe artifact path unavailable")?;
    if !existing.starts_with(&root) {
        return Err("probe artifact path is outside validation evidence".into());
    }
    Ok(())
}

pub fn main() -> Result<(), String> {
    let mode = std::env::var("MC_RELEASE_PROBE_MODE").map_err(|_| "set MC_RELEASE_PROBE_MODE")?;
    if !matches!(
        mode.as_str(),
        "capabilities" | "prepare" | "execute" | "resume"
    ) {
        return Err("unsupported probe mode".into());
    }
    if mode == "execute" && std::env::var("MC_RELEASE_PROBE_ONE_RENDER").as_deref() != Ok("1") {
        return Err("set MC_RELEASE_PROBE_ONE_RENDER=1 for the approved one-job run".into());
    }
    let report_path = PathBuf::from(
        std::env::var("MC_RELEASE_PROBE_REPORT")
            .map_err(|_| "set MC_RELEASE_PROBE_REPORT to an absolute fresh path")?,
    );
    if !report_path.is_absolute() {
        return Err("report path must be absolute".into());
    }
    validate_release_artifact_path(&report_path)?;
    let mut context = crate::app_context();
    if context.config().identifier != APP_ID {
        return Err("validation app identifier required".into());
    }
    context.config_mut().app.windows.clear();
    let app = tauri::Builder::default()
        .build(context)
        .map_err(|_| "validation app could not start")?;
    let handle = app.handle();
    if handle.config().identifier != APP_ID
        || handle
            .path()
            .app_data_dir()
            .map_err(|_| "app data path unavailable")?
            != Path::new(APP_DATA)
    {
        return Err("validation app data path required".into());
    }
    let service = app_inference_service(handle)?;
    let config =
        read_inference_config(handle).map_err(|_| "validation cloud configuration unavailable")?;
    if mode == "capabilities" {
        let (provider, profile_id) = selected_modal(&config)?;
        let client = service
            .build_http_client(provider, &profile_id, &config)
            .map_err(|_| "validation runtime credential unavailable")?;
        let recipe = gateway_recipe(&client)?;
        write_new_json(
            &report_path,
            &json!({
                "kind": "release_probe_capabilities", "appId": APP_ID,
                "renderProtocol": PROTOCOL_VERSION, "provider": "modal", "recipe": recipe,
                "gatewayCurrent": true,
            }),
        )?;
        return Ok(());
    }
    if !crate::inference::cloud_allowed(handle) {
        return Err("cloud is disabled in validation app".into());
    }
    let chapter_id = env_required("MC_RELEASE_PROBE_CHAPTER_ID")?;
    let page_index = env_required("MC_RELEASE_PROBE_PAGE_INDEX")?
        .parse::<u32>()
        .map_err(|_| "invalid probe page index")?;
    let region_id = env_required("MC_RELEASE_PROBE_REGION_ID")?;
    let job_path = Library::for_app(handle)
        .map_err(|_| "validation library unavailable")?
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
    let source_idx = Library::resolve_page(&job.project, page_index as usize)
        .ok_or("validation page unavailable")?;
    let source_path = job
        .source_path(source_idx)
        .ok_or("validation source unavailable")?;
    if !source_path
        .canonicalize()
        .map_err(|_| "validation source unavailable")?
        .starts_with(Path::new(APP_DATA))
    {
        return Err("source is outside validation app data".into());
    }
    if mode != "resume"
        && (!job
            .project
            .detections
            .iter()
            .any(|row| row.id == region_id && row.source_idx == source_idx)
            || job.project.patches.iter().any(|row| row.id == region_id))
    {
        return Err("probe target must be one stored detection without a patch".into());
    }
    if mode == "resume" {
        return resume(
            &service,
            &config,
            &job_path,
            &report_path,
            &chapter_id,
            page_index,
            &region_id,
        );
    }
    let (provider, profile_id) = selected_modal(&config)?;
    let client = service
        .build_http_client(provider, &profile_id, &config)
        .map_err(|_| "validation runtime credential unavailable")?;
    let recipe = gateway_recipe(&client)?;
    let intent = OperationIntent::CloudClean;
    let proposal = service
        .prepare_proposal(
            PrepareProposalRequest {
                chapter_id: chapter_id.clone(),
                page_index,
                region_id: Some(region_id.clone()),
                target: ExecutionTarget::Modal {
                    profile_id: profile_id.clone(),
                },
                recipe: recipe.clone(),
                intent: intent.clone(),
            },
            &config,
            true,
            &job_path,
        )
        .map_err(|_| "production consent preparation failed")?;
    let price = one_gpu_price(&client)?;
    let approval = approval_terms(&proposal, &price);
    let digest =
        sha256_hex(&serde_json::to_vec(&approval).map_err(|_| "approval encoding failed")?);
    if mode == "prepare" {
        write_new_json(
            &report_path,
            &json!({
                "kind": "release_probe_plan", "approvalDigest": digest, "approval": approval,
                "note": "One billable render maximum. Execute only with this exact digest after review.",
            }),
        )?;
        return Ok(());
    }
    let approved = env_required("MC_RELEASE_PROBE_APPROVE_DIGEST")?;
    let saved = read_plan(&report_path)?;
    if saved["approvalDigest"] != digest || saved["approval"] != approval || approved != digest {
        return Err("approved probe plan differs from current input, deployment or price".into());
    }
    let grant = service
        .confirm_proposal(ConfirmProposalOptions {
            proposal_id: &proposal.proposal_id,
            intent: &intent,
            config: &config,
            cloud_allowed: true,
            job_path: &job_path,
            grant_ttl: Duration::from_secs(120),
            max_attempts: 1,
        })
        .map_err(|_| "production consent confirmation failed")?;
    let input = read_render_input(&job_path, page_index, &region_id, &recipe)
        .map_err(|_| "probe input changed")?;
    let scope = input.scope(
        provider,
        &profile_id,
        &proposal.canonical_endpoint_fingerprint,
        compute_operation_digest(&intent).map_err(|_| "operation digest failed")?,
        &recipe,
        &region_id,
    );
    let dispatch_epoch = GrantService::global().get_profile_epoch(provider, &profile_id);
    GrantService::global()
        .validate_and_consume(&grant.nonce, &scope)
        .map_err(|_| "production grant did not match probe input")?;
    let execution_path = execution_path(&report_path);
    let mut execution = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&execution_path)
        .map_err(|_| "fresh execution report required")?;
    let attempt_id = attempt_id_for_nonce(&grant.nonce);
    append(
        &mut execution,
        &json!({"stage": "authorized", "attemptId": attempt_id,
        "approvalDigest": digest, "chapterId": chapter_id, "pageIndex": page_index,
        "regionId": region_id}),
    )?;
    let request = request_metadata(&region_id, page_index, &attempt_id, &recipe, &input);
    let journal = service.journal();
    let intent_record = CreateAttemptIntent {
        qwen_edit: None,
        attempt_id: attempt_id.clone(),
        job_id: request.job_id.clone(),
        provider,
        profile_id: profile_id.clone(),
        endpoint_fingerprint: proposal.canonical_endpoint_fingerprint.clone(),
        recipe_id: recipe.recipe_id.clone(),
        preprocessing_version: recipe.preprocessing_version.clone(),
        model_id: recipe.model_id.clone(),
        model_revision: recipe.model_revision.clone(),
        native_mask_conditioning: recipe.native_mask_conditioning,
        width: request.width,
        height: request.height,
        seed: request.seed,
        steps: request.steps,
        guidance_scaled: request.guidance_scaled,
        source_image_hash: input.source_hash.clone(),
        region_id: region_id.clone(),
        region_revision: input.revision,
        input_sha256: Some(input.input_sha256.clone()),
        predecessors_sha256: Some(input.predecessors_sha256.clone()),
        crop_sha256: input.crop_sha256.clone(),
        hint_sha256: input.hint_sha256.clone(),
        request_digest: request.request_digest.clone(),
        chapter_id: Some(chapter_id.clone()),
        page_index: Some(page_index),
    };
    let (_, guard) = journal
        .create_intent(intent_record)
        .map_err(|_| "attempt intent could not be recorded")?;
    journal
        .mark_dispatching(&guard)
        .map_err(|_| "attempt could not enter dispatching")?;
    drop(guard);
    if !matches!(still_selected_modal(handle, &config, &profile_id), Ok(true))
        || GrantService::global().get_profile_epoch(provider, &profile_id) != dispatch_epoch
    {
        let guard = journal
            .acquire_lock(&attempt_id)
            .map_err(|_| "attempt lock unavailable")?;
        journal
            .record_not_enqueued(&guard, "validation profile changed before POST")
            .map_err(|_| "attempt refusal could not be recorded")?;
        return Err("validation cloud profile changed before POST".into());
    }
    let current_terms = (|| {
        let info = client
            .get_model_info()
            .map_err(|_| "gateway model identity unavailable")?;
        let recipe = RenderRecipe::new(
            info.recipe_id,
            info.preprocessing_version,
            info.model_id,
            info.model_revision,
            info.native_mask_conditioning,
        );
        Ok::<_, String>((recipe, one_gpu_price(&client)?))
    })();
    if !matches!(current_terms, Ok((current_recipe, current_price))
        if current_recipe == recipe && current_price == price)
    {
        let guard = journal
            .acquire_lock(&attempt_id)
            .map_err(|_| "attempt lock unavailable")?;
        journal
            .record_not_enqueued(&guard, "validation deployment changed before POST")
            .map_err(|_| "attempt refusal could not be recorded")?;
        return Err("validation deployment changed before POST".into());
    }
    append(
        &mut execution,
        &json!({"stage": "post_attempted", "attemptId": attempt_id,
        "postCount": 1}),
    )?;
    let sent = client.submit_job(&request, &input.crop_png, &input.hint_png, journal.limits());
    let guard = journal
        .acquire_lock(&attempt_id)
        .map_err(|_| "attempt lock unavailable")?;
    match sent {
        Ok(accepted) => {
            drop(accepted);
            journal
                .record_dispatch_transport_error(&guard)
                .map_err(|_| "unknown phase could not be recorded")?;
        }
        Err(error) if error.definitely_not_enqueued() => {
            journal
                .record_not_enqueued(&guard, "probe POST was not enqueued")
                .map_err(|_| "not-enqueued phase could not be recorded")?;
            return Err("probe POST was not enqueued".into());
        }
        Err(_) => {
            journal
                .record_dispatch_transport_error(&guard)
                .map_err(|_| "unknown phase could not be recorded")?;
        }
    }
    drop(guard);
    append(
        &mut execution,
        &json!({"stage": "unknown", "attemptId": attempt_id}),
    )?;
    recover_until_committed(&service, &config, &job_path, &attempt_id, &mut execution)?;
    verify_one_patch(&service, &job_path, &attempt_id, &region_id, &mut execution)
}

fn env_required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing {name}"))
}

fn selected_modal(config: &InferenceConfig) -> Result<(CloudProvider, String), String> {
    match &config.selected_target {
        ExecutionTarget::Modal { profile_id } if config.modal_profiles.contains_key(profile_id) => {
            Ok((CloudProvider::Modal, profile_id.clone()))
        }
        _ => Err("select a Modal profile in the validation app".into()),
    }
}

fn gateway_recipe(
    client: &crate::inference::http::CloudHttpClient,
) -> Result<RenderRecipe, String> {
    if !client
        .gateway_is_current()
        .map_err(|_| "gateway capability handshake failed")?
    {
        return Err("gateway protocol or operations do not match this build".into());
    }
    let info = client
        .get_model_info()
        .map_err(|_| "gateway model identity unavailable")?;
    Ok(RenderRecipe::new(
        info.recipe_id,
        info.preprocessing_version,
        info.model_id,
        info.model_revision,
        info.native_mask_conditioning,
    ))
}

fn still_selected_modal(
    app: &tauri::AppHandle,
    old: &InferenceConfig,
    profile_id: &str,
) -> Result<bool, String> {
    if !crate::inference::cloud_allowed(app) {
        return Ok(false);
    }
    let current =
        read_inference_config(app).map_err(|_| "validation cloud configuration changed")?;
    if current.selected_target != old.selected_target {
        return Ok(false);
    }
    let (Some(before), Some(after)) = (
        old.modal_profiles.get(profile_id),
        current.modal_profiles.get(profile_id),
    ) else {
        return Ok(false);
    };
    Ok(compute_canonical_endpoint_fingerprint(&before.endpoint_url)
        .map_err(|_| "validation endpoint invalid")?
        == compute_canonical_endpoint_fingerprint(&after.endpoint_url)
            .map_err(|_| "validation endpoint changed")?)
}

fn one_gpu_price(
    client: &crate::inference::http::CloudHttpClient,
) -> Result<(String, f64), String> {
    let status = client
        .get_gpu_status()
        .map_err(|_| "GPU price unavailable")?
        .ok_or("GPU price unavailable")?;
    let mut prices = status.list_price_usd_per_hour.into_iter();
    let one = prices.next().ok_or("GPU price unavailable")?;
    if prices.next().is_some() || !one.1.is_finite() || one.1 <= 0.0 {
        return Err("one priced GPU is required for this probe".into());
    }
    Ok(one)
}

fn approval_terms(
    proposal: &crate::inference::consent::ConsentProposal,
    price: &(String, f64),
) -> Value {
    let mut terms = serde_json::to_value(proposal).expect("consent proposal serializes");
    let map = terms
        .as_object_mut()
        .expect("consent proposal is an object");
    for key in [
        "proposalId",
        "profileName",
        "endpointUrl",
        "issuedAtMs",
        "expiresAtMs",
    ] {
        map.remove(key);
    }
    let long = u64::from(proposal.crop_width.max(proposal.crop_height));
    let work_pixels = if long >= 768 {
        u64::from(proposal.crop_width) * u64::from(proposal.crop_height)
    } else {
        let snap = |extent: u32| {
            let scaled = (f64::from(extent) * 768.0 / long.max(1) as f64).round() as u64;
            (scaled - scaled % 16).max(16)
        };
        snap(proposal.crop_width) * snap(proposal.crop_height)
    };
    let gpu_seconds = 2.0 + work_pixels as f64 / 1e6 * 10.0;
    let per_second = price.1 / 3600.0 + 2.0 * 0.000_013_1 + 24.0 * 0.000_002_22;
    map.insert(
        "gpu".into(),
        json!({"name": price.0, "listPriceUsdPerHour": price.1}),
    );
    map.insert(
        "estimate".into(),
        json!({"workPixels": work_pixels,
        "gpuSecondsLow": gpu_seconds, "gpuSecondsHigh": gpu_seconds + 55.0 + 600.0,
        "costUsdLow": gpu_seconds * per_second,
        "costUsdHigh": (gpu_seconds + 55.0 + 600.0) * per_second,
        "basis": "unmeasured render-time estimate with 24 GiB memory allowance"}),
    );
    terms
}

fn request_metadata(
    region_id: &str,
    page_index: u32,
    attempt_id: &str,
    recipe: &RenderRecipe,
    input: &crate::inference::service::RenderInput,
) -> JobRequestMetadata {
    let sampling = wire_sampling();
    let mut request = JobRequestMetadata {
        protocol_version: PROTOCOL_VERSION.into(),
        job_id: format!(
            "job-{}",
            &sha256_hex(format!("{region_id}:{page_index}").as_bytes())[..24]
        ),
        attempt_id: attempt_id.into(),
        recipe: WireRenderRecipe::from(recipe.clone()),
        width: input.crop_bounds.w,
        height: input.crop_bounds.h,
        seed: sampling.seed,
        steps: sampling.steps,
        guidance_scaled: sampling.guidance_scaled,
        image_sha256: input.crop_sha256.clone(),
        hint_sha256: input.hint_sha256.clone(),
        request_digest: String::new(),
    };
    request.request_digest = compute_request_digest(&request);
    request
}

fn execution_path(report: &Path) -> PathBuf {
    report.with_extension("execution.jsonl")
}

fn write_new_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "fresh report output required")?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|_| "report encoding failed")?;
    file.write_all(b"\n").map_err(|_| "report write failed")?;
    file.sync_all().map_err(|_| "report sync failed".into())
}

fn append(file: &mut File, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *file, value).map_err(|_| "execution report encoding failed")?;
    file.write_all(b"\n")
        .map_err(|_| "execution report write failed")?;
    file.sync_all()
        .map_err(|_| "execution report sync failed".into())
}

fn read_plan(path: &Path) -> Result<Value, String> {
    let value: Value =
        serde_json::from_slice(&fs::read(path).map_err(|_| "probe plan unavailable")?)
            .map_err(|_| "probe plan unreadable")?;
    if value["kind"] != "release_probe_plan" || !value["approvalDigest"].is_string() {
        return Err("probe plan is invalid".into());
    }
    Ok(value)
}

fn resume(
    service: &InferenceService,
    config: &InferenceConfig,
    job_path: &Path,
    report_path: &Path,
    chapter_id: &str,
    page_index: u32,
    region_id: &str,
) -> Result<(), String> {
    let saved = read_plan(report_path)?;
    let execution_path = execution_path(report_path);
    let first =
        BufReader::new(File::open(&execution_path).map_err(|_| "execution report unavailable")?)
            .lines()
            .next()
            .ok_or("execution report is empty")?
            .map_err(|_| "execution report unreadable")?;
    let first: Value = serde_json::from_str(&first).map_err(|_| "execution report invalid")?;
    if first["approvalDigest"] != saved["approvalDigest"]
        || first["chapterId"] != chapter_id
        || first["pageIndex"] != page_index
        || first["regionId"] != region_id
    {
        return Err("execution report does not match probe target".into());
    }
    let attempt_id = first["attemptId"]
        .as_str()
        .ok_or("attempt identity missing")?;
    let record = service
        .journal()
        .get_record(attempt_id)
        .map_err(|_| "attempt journal unavailable")?;
    if record.chapter_id.as_deref() != Some(chapter_id)
        || record.page_index != Some(page_index)
        || record.region_id != region_id
        || record.provider != CloudProvider::Modal
    {
        return Err("attempt journal does not match probe target".into());
    }
    let mut execution = OpenOptions::new()
        .append(true)
        .open(&execution_path)
        .map_err(|_| "execution report unavailable")?;
    recover_until_committed(service, config, job_path, attempt_id, &mut execution)?;
    verify_one_patch(service, job_path, attempt_id, region_id, &mut execution)
}

fn recover_until_committed(
    service: &InferenceService,
    config: &InferenceConfig,
    job_path: &Path,
    attempt_id: &str,
    execution: &mut File,
) -> Result<(), String> {
    let began = Instant::now();
    let polls = PollOptions {
        poll_interval: Duration::from_secs(1),
        timeout: Duration::from_secs(20),
        max_polls: 20,
    };
    loop {
        let decision = service
            .recover_cloud_attempt(attempt_id, job_path, config, &polls)
            .map_err(|error| format!("recovery failed: {}", error.code()))?;
        match decision {
            RecoveryDecision::AlreadyCommitted { .. } => {
                append(
                    execution,
                    &json!({"stage": "committed", "attemptId": attempt_id}),
                )?;
                return Ok(());
            }
            RecoveryDecision::AmbiguousUnknown { .. } | RecoveryDecision::ResumePolling { .. } => {}
            _ => return Err("attempt ended without a committed patch".into()),
        }
        if began.elapsed() >= MAX_RECOVERY {
            append(
                execution,
                &json!({"stage": "pending", "attemptId": attempt_id}),
            )?;
            return Err("recovery still pending; use resume mode for GET-only continuation".into());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn verify_one_patch(
    service: &InferenceService,
    job_path: &Path,
    attempt_id: &str,
    region_id: &str,
    execution: &mut File,
) -> Result<(), String> {
    for _ in 0..2 {
        if !matches!(
            service
                .recover_attempt_locally(attempt_id, Some(job_path))
                .map_err(|error| format!("repeat recovery failed: {}", error.code()))?,
            RecoveryDecision::AlreadyCommitted { .. }
        ) {
            return Err("repeat recovery did not find the committed attempt".into());
        }
    }
    let job = Job::open(job_path).map_err(|_| "validation chapter unreadable")?;
    let patches: Vec<_> = job
        .project
        .patches
        .iter()
        .filter(|row| row.id == region_id)
        .collect();
    if patches.len() != 1
        || patches[0]
            .provenance
            .cloud
            .as_ref()
            .and_then(|cloud| cloud.attempt_id.as_deref())
            != Some(attempt_id)
        || job.project.detections.iter().any(|row| row.id == region_id)
        || !matches!(
            service
                .journal()
                .get_record(attempt_id)
                .map_err(|_| "attempt journal unreadable")?
                .phase,
            AttemptPhase::Committed { .. }
        )
    {
        return Err("one-patch recovery verification failed".into());
    }
    append(
        execution,
        &json!({"stage": "verified", "attemptId": attempt_id,
        "renderPostCount": 1, "targetPatchCount": 1, "repeatRecoveries": 2}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::consent::ConsentProposal;
    use cleaner_core::mask::Rect;

    #[test]
    fn artifact_guard_accepts_new_descendant_and_rejects_library_or_traversal() {
        let scratch = std::env::temp_dir().join(format!(
            "mc-artifact-guard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = scratch.join("evidence");
        fs::create_dir_all(&root).unwrap();
        assert!(
            validate_artifact_path_with_root(&root.join("new/nested/report.json"), &root).is_ok()
        );
        assert!(validate_artifact_path_with_root(
            &Path::new(APP_DATA).join("cloud_attempts/probe.json"),
            &root
        )
        .is_err());
        assert!(validate_artifact_path_with_root(&root.join("../outside.json"), &root).is_err());
        fs::remove_dir_all(scratch).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn artifact_guard_rejects_existing_symlink_escape() {
        use std::os::unix::fs::symlink;
        let scratch = std::env::temp_dir().join(format!(
            "mc-artifact-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = scratch.join("evidence");
        let outside = scratch.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("link")).unwrap();
        assert!(
            validate_artifact_path_with_root(&root.join("link/new/report.json"), &root).is_err()
        );
        fs::remove_dir_all(scratch).unwrap();
    }

    fn proposal() -> ConsentProposal {
        ConsentProposal {
            proposal_id: "expires-with-process".into(),
            provider: CloudProvider::Modal,
            profile_id: "validation".into(),
            profile_name: "Display name".into(),
            endpoint_url: "https://example.invalid".into(),
            canonical_endpoint_fingerprint: "endpoint-fingerprint".into(),
            crop_bounds: Rect::new(1, 2, 32, 32),
            crop_width: 32,
            crop_height: 32,
            crop_png_sha256: "crop".into(),
            hint_png_sha256: "hint".into(),
            operation_digest: "operation".into(),
            source_hash: "source".into(),
            mask_hash: "mask".into(),
            revision: 1,
            revision_hash: "revision".into(),
            input_sha256: "input".into(),
            predecessors_sha256: "predecessors".into(),
            recipe: RenderRecipe::new("recipe", "preprocess", "model", "revision", true),
            region_ids: vec!["region".into()],
            intent: OperationIntent::CloudClean,
            chapter_id: "chapter".into(),
            page_index: 0,
            source_idx: 0,
            issued_at_ms: 10,
            expires_at_ms: 20,
        }
    }

    #[test]
    fn approval_reprepare_ignores_expiry_and_display_but_binds_price_and_input() {
        let first = approval_terms(&proposal(), &("A100".into(), 2.0));
        let mut renewed = proposal();
        renewed.proposal_id = "renewed".into();
        renewed.profile_name = "Renamed".into();
        renewed.endpoint_url = "https://other.invalid".into();
        renewed.issued_at_ms = 100;
        renewed.expires_at_ms = 200;
        assert_eq!(first, approval_terms(&renewed, &("A100".into(), 2.0)));
        assert!(first.get("proposalId").is_none());
        assert!(first.get("endpointUrl").is_none());
        assert_ne!(first, approval_terms(&renewed, &("A100".into(), 3.0)));
        renewed.input_sha256 = "changed input".into();
        assert_ne!(first, approval_terms(&renewed, &("A100".into(), 2.0)));
    }
}
