//! Debug-only pre-enqueue refusal probe for one stored validation fixture.
//! Prepare writes a reviewable plan. Execute sends at most one old-recipe POST.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
use crate::inference::journal::CreateAttemptIntent;
use crate::inference::policy::GrantService;
use crate::inference::service::{attempt_id_for_nonce, read_render_input, RenderInput};
use crate::library::Library;

const APP_ID: &str = "com.mangacleaner.validation20260927";
const APP_DATA: &str =
    "/Users/caved/Library/Application Support/com.mangacleaner.validation20260927";
const OLD_VERSION: &str = "1.0.0";
const CURRENT_VERSION: &str = "2.0.0";

pub fn main() -> Result<(), String> {
    let mode = required("MC_RELEASE_RECIPE_MODE")?;
    if !matches!(mode.as_str(), "prepare" | "execute") {
        return Err("unsupported recipe probe mode".into());
    }
    let report = PathBuf::from(required("MC_RELEASE_RECIPE_REPORT")?);
    if !report.is_absolute() {
        return Err("report path must be absolute".into());
    }
    crate::release_probe::validate_release_artifact_path(&report)?;
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
    if !crate::inference::cloud_allowed(handle) {
        return Err("cloud is disabled in validation app".into());
    }
    let chapter_id = required("MC_RELEASE_RECIPE_CHAPTER_ID")?;
    let page_index = required("MC_RELEASE_RECIPE_PAGE_INDEX")?
        .parse::<u32>()
        .map_err(|_| "invalid page index")?;
    let region_id = required("MC_RELEASE_RECIPE_REGION_ID")?;
    let library = Library::for_app(handle).map_err(|_| "validation library unavailable")?;
    let job_path = library
        .resolve_chapter(&chapter_id)
        .map_err(|_| "validation chapter unavailable")?;
    inside_validation(&job_path)?;
    let job = Job::open(&job_path).map_err(|_| "validation chapter unreadable")?;
    let source_idx = Library::resolve_page(&job.project, page_index as usize)
        .ok_or("validation page unavailable")?;
    let source = job
        .source_path(source_idx)
        .ok_or("validation source unavailable")?;
    inside_validation(&source)?;
    if !job
        .project
        .detections
        .iter()
        .any(|row| row.id == region_id && row.source_idx == source_idx)
        || job.project.patches.iter().any(|row| row.id == region_id)
    {
        return Err("probe requires one stored detection without a patch".into());
    }
    let config =
        read_inference_config(handle).map_err(|_| "validation cloud configuration unavailable")?;
    let profile_id = selected_modal(&config)?;
    let profile_epoch = GrantService::global().get_profile_epoch(CloudProvider::Modal, &profile_id);
    let service = app_inference_service(handle)?;
    let client = service
        .build_http_client(CloudProvider::Modal, &profile_id, &config)
        .map_err(|_| "validation runtime credential unavailable")?;
    if !client
        .gateway_is_current()
        .map_err(|_| "gateway capability handshake failed")?
    {
        return Err("gateway protocol or operations do not match this build".into());
    }
    let current = current_recipe(&client)?;
    if current.preprocessing_version != CURRENT_VERSION {
        return Err("gateway must advertise preprocessing 2.0.0".into());
    }
    let mut old = current.clone();
    old.preprocessing_version = OLD_VERSION.into();
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
                recipe: old.clone(),
                intent: intent.clone(),
            },
            &config,
            true,
            &job_path,
        )
        .map_err(|_| "production consent preparation failed")?;
    let input = read_render_input(&job_path, page_index, &region_id, &old)
        .map_err(|_| "old-recipe input unavailable")?;
    let approval = approval(
        (&chapter_id, page_index, &region_id),
        &profile_id,
        &proposal.canonical_endpoint_fingerprint,
        &current,
        &old,
        &input,
    );
    let digest =
        sha256_hex(&serde_json::to_vec(&approval).map_err(|_| "approval encoding failed")?);
    if mode == "prepare" {
        write_new(
            &report,
            &json!({"kind":"release_recipe_probe_plan", "approvalDigest":digest,
            "approval":approval, "note":"One POST maximum; expected pre-enqueue refusal. An unexpected acceptance can incur GPU charges."}),
        )?;
        return Ok(());
    }
    if required("MC_RELEASE_RECIPE_APPROVE_DIGEST")? != digest {
        return Err("approval digest differs from current input".into());
    }
    if required("MC_RELEASE_RECIPE_ONE_POST")? != "1" {
        return Err("explicit one-POST authorization required".into());
    }
    let saved: Value =
        serde_json::from_slice(&fs::read(&report).map_err(|_| "probe plan unavailable")?)
            .map_err(|_| "probe plan unreadable")?;
    if saved["kind"] != "release_recipe_probe_plan"
        || saved["approvalDigest"] != digest
        || saved["approval"] != approval
    {
        return Err("approved plan differs from current fixture or deployment".into());
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
    let scope = input.scope(
        CloudProvider::Modal,
        &profile_id,
        &proposal.canonical_endpoint_fingerprint,
        compute_operation_digest(&intent).map_err(|_| "operation digest failed")?,
        &old,
        &region_id,
    );
    GrantService::global()
        .validate_and_consume(&grant.nonce, &scope)
        .map_err(|_| "production grant did not match probe input")?;
    let attempt_id = attempt_id_for_nonce(&grant.nonce);
    let request = request_metadata(&region_id, page_index, &attempt_id, &old, &input);
    let execution_path = report.with_extension("execution.jsonl");
    let mut execution = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&execution_path)
        .map_err(|_| "fresh single-use execution report required")?;
    append(
        &mut execution,
        &json!({"stage":"authorized", "approvalDigest":digest,
        "attemptId":attempt_id, "requestDigest":request.request_digest}),
    )?;
    let journal = service.journal();
    let (_, guard) = journal
        .create_intent(CreateAttemptIntent {
            qwen_edit: None,
            attempt_id: attempt_id.clone(),
            job_id: request.job_id.clone(),
            provider: CloudProvider::Modal,
            profile_id: profile_id.clone(),
            endpoint_fingerprint: proposal.canonical_endpoint_fingerprint.clone(),
            recipe_id: old.recipe_id.clone(),
            preprocessing_version: old.preprocessing_version.clone(),
            model_id: old.model_id.clone(),
            model_revision: old.model_revision.clone(),
            native_mask_conditioning: old.native_mask_conditioning,
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
            chapter_id: Some(chapter_id),
            page_index: Some(page_index),
        })
        .map_err(|_| "attempt intent could not be recorded")?;
    journal
        .mark_dispatching(&guard)
        .map_err(|_| "attempt could not enter dispatching")?;
    drop(guard);
    // The second authenticated read closes ordinary deployment drift before
    // the only POST. A concurrent redeploy between these calls still requires
    // operator control; the gateway's recipe check is the enqueue boundary.
    let unchanged = read_inference_config(handle)
        .ok()
        .and_then(|latest_config| {
            let latest_profile = latest_config.modal_profiles.get(&profile_id)?;
            let latest_endpoint =
                compute_canonical_endpoint_fingerprint(&latest_profile.endpoint_url).ok()?;
            Some(
                selected_modal(&latest_config).ok()? == profile_id
                    && latest_endpoint == proposal.canonical_endpoint_fingerprint,
            )
        })
        .unwrap_or(false);
    if current_recipe(&client).ok().as_ref() != Some(&current)
        || !crate::inference::cloud_allowed(handle)
        || !unchanged
        || GrantService::global().get_profile_epoch(CloudProvider::Modal, &profile_id)
            != profile_epoch
    {
        let guard = journal
            .acquire_lock(&attempt_id)
            .map_err(|_| "attempt lock unavailable")?;
        journal
            .record_not_enqueued(&guard, "validation deployment changed before POST")
            .map_err(|_| "pre-POST refusal could not be recorded")?;
        return Err("validation deployment changed before POST".into());
    }
    append(
        &mut execution,
        &json!({"stage":"post_attempted", "attemptId":attempt_id, "postCount":1}),
    )?;
    let outcome =
        client.submit_job_probe(&request, &input.crop_png, &input.hint_png, journal.limits());
    let guard = journal
        .acquire_lock(&attempt_id)
        .map_err(|_| "attempt lock unavailable")?;
    match outcome {
        Err(error) if error.exact_unsupported_recipe && error.error.definitely_not_enqueued() => {
            journal
                .record_not_enqueued(&guard, "bound unsupported_recipe pre-enqueue refusal")
                .map_err(|_| "refusal could not be recorded")?;
            append(
                &mut execution,
                &json!({"stage":"verified_pre_enqueue_refusal", "code":"unsupported_recipe",
                "attemptId":attempt_id, "postCount":1}),
            )?;
            Ok(())
        }
        Ok(accepted) => {
            journal
                .record_accepted_handle(&guard, &accepted.handle)
                .map_err(|_| "accepted handle could not be recorded")?;
            append(
                &mut execution,
                &json!({"stage":"unexpected_accepted", "attemptId":attempt_id,
                "handle":accepted.handle, "postCount":1}),
            )?;
            Err("gateway unexpectedly enqueued old-recipe request; no recovery or retry was started".into())
        }
        Err(error) => {
            if error.error.definitely_not_enqueued() {
                journal
                    .record_not_enqueued(&guard, "unexpected pre-enqueue refusal")
                    .map_err(|_| "refusal could not be recorded")?;
            } else {
                journal
                    .record_dispatch_transport_error(&guard)
                    .map_err(|_| "unknown transport phase could not be recorded")?;
            }
            append(
                &mut execution,
                &json!({"stage":"unverified", "attemptId":attempt_id,
                "postCount":1, "definitelyNotEnqueued":error.error.definitely_not_enqueued()}),
            )?;
            Err("old-recipe refusal was not proven; no retry was started".into())
        }
    }
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing {name}"))
}

fn inside_validation(path: &Path) -> Result<(), String> {
    if !path
        .canonicalize()
        .map_err(|_| "validation path unavailable")?
        .starts_with(Path::new(APP_DATA))
    {
        return Err("path is outside validation app data".into());
    }
    Ok(())
}

fn selected_modal(config: &InferenceConfig) -> Result<String, String> {
    match &config.selected_target {
        ExecutionTarget::Modal { profile_id } if config.modal_profiles.contains_key(profile_id) => {
            Ok(profile_id.clone())
        }
        _ => Err("select a Modal profile in the validation app".into()),
    }
}

fn current_recipe(
    client: &crate::inference::http::CloudHttpClient,
) -> Result<RenderRecipe, String> {
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

fn approval(
    fixture: (&str, u32, &str),
    profile_id: &str,
    endpoint: &str,
    current: &RenderRecipe,
    old: &RenderRecipe,
    input: &RenderInput,
) -> Value {
    let (chapter_id, page_index, region_id) = fixture;
    json!({"chapterId":chapter_id, "pageIndex":page_index, "regionId":region_id,
        "provider":"modal", "profileId":profile_id, "endpointFingerprint":endpoint,
        "currentRecipe":current, "requestedRecipe":old, "cropSha256":input.crop_sha256,
        "hintSha256":input.hint_sha256, "sourceSha256":input.source_hash,
        "inputSha256":input.input_sha256, "predecessorsSha256":input.predecessors_sha256,
        "maxPosts":1, "expected":"bound unsupported_recipe before GPU enqueue"})
}

fn request_metadata(
    region_id: &str,
    page_index: u32,
    attempt_id: &str,
    recipe: &RenderRecipe,
    input: &RenderInput,
) -> JobRequestMetadata {
    let sampling = wire_sampling();
    let mut request = JobRequestMetadata {
        protocol_version: PROTOCOL_VERSION.into(),
        job_id: format!(
            "job-{}",
            &sha256_hex(format!("old:{region_id}:{page_index}").as_bytes())[..24]
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

fn write_new(path: &Path, value: &Value) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| "fresh plan output required")?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|_| "plan encoding failed")?;
    file.write_all(b"\n").map_err(|_| "plan write failed")?;
    file.sync_all().map_err(|_| "plan sync failed".into())
}

fn append(file: &mut File, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *file, value).map_err(|_| "execution encoding failed")?;
    file.write_all(b"\n")
        .map_err(|_| "execution write failed")?;
    file.sync_all().map_err(|_| "execution sync failed".into())
}
