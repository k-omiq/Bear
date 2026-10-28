//! A cloud result finished again: one region's saved answer composited by
//! another finishing, with no request and no GPU.
//!
//! A cloud clean keeps the model's raw answer with its attempt
//! (`journal.rs`, `result.png`), and the finishing is the client's own step
//! after it ([`cleaner_core::engines::postprocess`]). So the finishing of a
//! region already cleaned can be changed after the fact: the region is
//! prepared again from the page under it, held to the input the attempt sent
//! ([`super::service::check_prepared_input`], the attach's own checks), and
//! the answer is composited again by the finishing picked.
//!
//! Only a cloud result can be: a local clean keeps no answer of its own, and
//! its region is cleaned again to change how it is finished. What cannot be
//! finished again is said as an [`Unavailable`] reason, which the editor's
//! panel shows in place of its previews.

use std::path::Path;

use cleaner_core::cloud_decode::DecodedResultCrop;
use cleaner_core::engines::model::Rendered;
use cleaner_core::engines::postprocess::{Choice, Finishing};
use cleaner_core::engines::render::Preprocessing;
use cleaner_core::ingest::sha256_hex;
use cleaner_core::patch::Patch;
use cleaner_core::project::{Job, PatchRecord};
use serde::Serialize;

use crate::inference::consent::{encode_rgb8_png, load_cloud_region, CloudRegionKind};
use crate::inference::journal::{AttemptJournal, AttemptPhase, JournalError};
use crate::library::{ApiRegion, Library};

/// The long side of a preview, in pixels: enough to tell the finishings
/// apart in the panel's grid, small enough that all of them cross the
/// seam as one answer.
pub const PREVIEW_LONG_SIDE: u32 = 256;

/// Why a region's finishing cannot be changed. The panel says which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Unavailable {
    /// Not a cloud result: cleaned on this computer, painted, or text-shaped.
    /// No answer was kept, so the region is cleaned again instead.
    NotCloud,
    /// The saved answer is gone, or cannot be read.
    NoResult,
    /// The page, or a layer under the region, changed since the answer came
    /// back: it would be composited onto a page it did not answer.
    PageChanged,
    /// Rendered under the first cloud preparation (1.0.0), whose region is
    /// the patch's own mask and moves when the patch is written, or under one
    /// this build does not know.
    Unsupported,
}

impl Unavailable {
    /// The code a refused change starts with, as every refusal at the seam
    /// does.
    fn refusal(self) -> String {
        match self {
            Unavailable::NotCloud => "refinish_not_cloud: the region was not cleaned in the cloud",
            Unavailable::NoResult => "refinish_no_result: the saved cloud answer is gone",
            Unavailable::PageChanged => "refinish_page_changed: the page under the region changed",
            Unavailable::Unsupported => "refinish_unsupported: the result's preparation cannot be finished again",
        }
        .to_owned()
    }
}

/// A cloud result prepared again for its finishing to be changed: the patch
/// as it stands, the preparation its answer was made for, and the answer.
pub(crate) struct Reopened {
    source_idx: usize,
    record: PatchRecord,
    patch: Patch,
    cloud: crate::underlay::CloudInput,
    answer: DecodedResultCrop,
    /// The finishing the patch's pixels were made by, from its record
    /// (`params_snapshot["postprocess"]`); `None` for a render finished by
    /// its version's own composite.
    current: Option<Choice>,
    /// Whether a colour answer is made grey: the current finishing's, or the
    /// attempt's when the patch records none.
    grayscale: bool,
}

/// The finishing a patch's record names, if it names one.
pub(crate) fn recorded_choice(params: &serde_json::Value) -> Option<Choice> {
    let postprocess = params.get("postprocess")?;
    let finishing = serde_json::from_value::<Finishing>(postprocess.get("finishing")?.clone()).ok()?;
    let grayscale = postprocess.get("grayscale").and_then(serde_json::Value::as_bool).unwrap_or(false);
    Some(Choice { finishing, grayscale })
}

/// Prepare region `region_id` of `job` again with its saved answer, or say
/// why it cannot be. The caller holds the job's lock. An error is a failure
/// to read, or an attempt another command is using; a refusal is `Ok(Err)`.
pub(crate) fn reopen(
    journal: &AttemptJournal,
    job: &Job,
    region_id: &str,
) -> Result<Result<Reopened, Unavailable>, String> {
    let record = job
        .project
        .patches
        .iter()
        .find(|row| row.id == region_id && row.visible)
        .ok_or_else(|| format!("refinish_no_patch: region {region_id} has no cleaned result"))?;
    let Some(attempt_id) = record.provenance.cloud.as_ref().and_then(|cloud| cloud.attempt_id.clone()) else {
        return Ok(Err(Unavailable::NotCloud));
    };
    if record.geometry_policy == cleaner_core::text_shape::GeometryPolicy::TextShape {
        return Ok(Err(Unavailable::NotCloud));
    }

    // The attempt that made this patch, committed to it, and its answer. Held
    // only while they are read: nothing here writes the attempt.
    let guard = match journal.acquire_lock(&attempt_id) {
        Ok(guard) => guard,
        Err(JournalError::AttemptLocked { .. }) => {
            return Err("refinish_busy: the cloud result is in use, try again".into());
        }
        Err(_) => return Ok(Err(Unavailable::NoResult)),
    };
    let attempt = match journal.read_record_locked(&guard) {
        Ok(attempt) => attempt,
        Err(_) => return Ok(Err(Unavailable::NoResult)),
    };
    if !matches!(&attempt.phase, AttemptPhase::Committed { patch_id, .. } if patch_id == region_id) {
        return Ok(Err(Unavailable::NoResult));
    }
    let Ok(answer) = journal.read_cached_crop(&guard) else {
        return Ok(Err(Unavailable::NoResult));
    };
    drop(guard);

    // The page the answer was made from, read as the attach read it.
    let Some(source_path) = job.source_path(record.source_idx) else {
        return Ok(Err(Unavailable::PageChanged));
    };
    let source = std::fs::read(source_path).map_err(|e| e.to_string())?;
    if record.provenance.source_sha256 != attempt.source_image_hash || sha256_hex(&source) != attempt.source_image_hash {
        return Ok(Err(Unavailable::PageChanged));
    }
    let preprocessing = match Preprocessing::of(&attempt.preprocessing_version) {
        Ok(Preprocessing::V1) | Err(_) => return Ok(Err(Unavailable::Unsupported)),
        Ok(preprocessing) => preprocessing,
    };
    let region = match load_cloud_region(job, region_id, &attempt.source_image_hash) {
        Ok(region) => region,
        Err(crate::inference::consent::ConsentError::RenderPrepareFailed(_)) => return Ok(Err(Unavailable::NotCloud)),
        Err(error) => return Err(error.to_string()),
    };
    let page = job.display_page(record.source_idx, &source).map_err(|e| e.to_string())?;
    // The region is a patch now, and a patch is prepared from the page under
    // it, as a cloud re-render of it is: the same preparation the attempt
    // sent, unless something under it changed.
    let Ok(cloud) = region.prepare(job, &page, preprocessing) else {
        return Ok(Err(Unavailable::PageChanged));
    };
    if super::service::check_prepared_input(&cloud, &attempt.snapshot(), attempt.width, attempt.height).is_err() {
        return Ok(Err(Unavailable::PageChanged));
    }
    let CloudRegionKind::Patch(stored) = region.kind else {
        return Ok(Err(Unavailable::NoResult));
    };
    let (record, patch) = *stored;
    let current = recorded_choice(&record.provenance.params_snapshot);
    let grayscale = current.or(attempt.finishing).is_some_and(|choice| choice.grayscale);
    Ok(Ok(Reopened {
        source_idx: record.source_idx,
        record,
        patch,
        cloud,
        answer,
        current,
        grayscale,
    }))
}

impl Reopened {
    /// The answer composited by `choice`, in page coordinates: what the patch
    /// would hold.
    fn rendered(&self, choice: Choice) -> Result<Rendered, String> {
        let mut prepared = self.cloud.prepared.clone();
        prepared.set_finishing(Some(choice));
        let mut rendered = prepared.composite(&self.answer.as_generated_crop()).map_err(|e| e.to_string())?;
        rendered.mask.bounds.x += self.cloud.input.window.x + self.cloud.source_shift.0;
        rendered.mask.bounds.y += self.cloud.input.window.y + self.cloud.source_shift.1;
        Ok(rendered)
    }

    /// The crop the model was shown with the answer finished by `choice` in
    /// it, as a PNG no longer than [`PREVIEW_LONG_SIDE`] on either side.
    fn preview(&self, choice: Choice) -> Result<Vec<u8>, String> {
        let mut prepared = self.cloud.prepared.clone();
        prepared.set_finishing(Some(choice));
        let (_, after) = prepared.preview_rgb8(&self.answer.as_generated_crop()).map_err(|e| e.to_string())?;
        let crop = prepared.crop();
        let (w, h) = (crop.w as usize, crop.h as usize);
        if after.len() != w * h * 3 {
            return Err("preview size does not match its crop".into());
        }
        let scale = f64::from(PREVIEW_LONG_SIDE) / f64::from(crop.w.max(crop.h));
        if scale >= 1.0 {
            return encode_rgb8_png(crop.w, crop.h, &after).map_err(|e| e.to_string());
        }
        // An area average per channel: a screentone shrunk by sampling would
        // show moire that no finishing made.
        use cleaner_core::engines::grid::{resize_area, Grid};
        let nw = ((w as f64 * scale).round() as usize).max(1);
        let nh = ((h as f64 * scale).round() as usize).max(1);
        let mut small = vec![0u8; nw * nh * 3];
        for channel in 0..3 {
            let grid = Grid { w, h, data: after.iter().skip(channel).step_by(3).map(|&v| f64::from(v)).collect() };
            for (i, v) in resize_area(&grid, nw, nh).data.into_iter().enumerate() {
                small[i * 3 + channel] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
        encode_rgb8_png(nw as u32, nh as u32, &small).map_err(|e| e.to_string())
    }

    /// One preview per finishing, in [`Finishing::ALL`]'s order, at this
    /// result's grey choice. Composited side by side: a large crop takes a
    /// second or more for each.
    fn previews(&self) -> Vec<(Finishing, Result<Vec<u8>, String>)> {
        let grayscale = self.grayscale;
        std::thread::scope(|scope| {
            let workers: Vec<_> = Finishing::ALL
                .iter()
                .map(|&finishing| (finishing, scope.spawn(move || self.preview(Choice { finishing, grayscale }))))
                .collect();
            workers
                .into_iter()
                .map(|(finishing, worker)| {
                    (finishing, worker.join().unwrap_or_else(|_| Err("the preview failed".into())))
                })
                .collect()
        })
    }
}

/// What the panel shows for a region: a preview per finishing and the one
/// its pixels were made by, or why there are none.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishingPreviews {
    pub region_id: String,
    /// `None` when the previews are there.
    pub unavailable: Option<Unavailable>,
    pub current: Option<Choice>,
    pub grayscale: bool,
    pub previews: Vec<FinishingPreview>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishingPreview {
    pub finishing: Finishing,
    /// A `data:image/png` URL, or `None` when this finishing failed on the
    /// answer: the others are still shown.
    pub url: Option<String>,
}

/// [`region_finishing_previews`] on a resolved chapter.
pub(crate) fn previews_at(journal: &AttemptJournal, job_path: &Path, region_id: &str) -> Result<FinishingPreviews, String> {
    // Read under the lock, composited after it: nothing is written, and a
    // long render must not hold the chapter.
    let reopened = {
        let _lock = crate::run::lock_job(job_path)?;
        let job = Job::open(job_path).map_err(|e| e.to_string())?;
        reopen(journal, &job, region_id)?
    };
    let reopened = match reopened {
        Ok(reopened) => reopened,
        Err(why) => {
            return Ok(FinishingPreviews {
                region_id: region_id.to_owned(),
                unavailable: Some(why),
                current: None,
                grayscale: false,
                previews: Vec::new(),
            });
        }
    };
    use base64::Engine as _;
    let previews = reopened
        .previews()
        .into_iter()
        .map(|(finishing, png)| FinishingPreview {
            finishing,
            url: match png {
                Ok(png) => Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))),
                Err(error) => {
                    eprintln!("a finishing preview of {region_id} failed: {error}");
                    None
                }
            },
        })
        .collect();
    Ok(FinishingPreviews {
        region_id: region_id.to_owned(),
        unavailable: None,
        current: reopened.current,
        grayscale: reopened.grayscale,
        previews,
    })
}

/// What a change of finishing answers: the region as it now stands, its
/// page's status, and the chapter's undo index after the step it recorded.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishingChange {
    pub region: ApiRegion,
    pub page_status: String,
    pub history: Option<crate::history::HistoryView>,
}

/// [`set_region_finishing`] on a resolved chapter: the patch's pixels and
/// mask replaced by the answer finished by `choice`, its post-processing
/// record with them, and one undo step, all under the page's lock. Every
/// other part of the patch - its layer settings, order, review state and
/// provenance - is kept. Asked for the finishing it has, nothing is written.
pub(crate) fn apply_at(
    journal: &AttemptJournal,
    job_path: &Path,
    region_id: &str,
    choice: Choice,
) -> Result<FinishingChange, String> {
    let chapter_id = super::service::chapter_id_of(job_path);
    let _lock = match crate::region::page_index_in(&chapter_id, region_id) {
        Some(page_index) => crate::run::lock_page(job_path, &chapter_id, page_index)?,
        None => crate::run::lock_job(job_path)?,
    };
    let mut job = Job::open(job_path).map_err(|e| e.to_string())?;
    let reopened = reopen(journal, &job, region_id)?.map_err(Unavailable::refusal)?;
    let source_idx = reopened.source_idx;
    let answer = |job: &Job, history| {
        crate::library::region_and_status(&chapter_id, &job.project, source_idx, region_id)
            .map(|(region, page_status)| FinishingChange { region, page_status, history })
            .ok_or_else(|| format!("refinish_no_patch: region {region_id} is not on its page"))
    };
    if reopened.current == Some(choice) {
        return answer(&job, None);
    }

    let before = crate::history::side_now(&chapter_id, &job.project, source_idx, region_id);
    let rendered = reopened.rendered(choice)?;
    let mut patch = reopened.patch;
    let params = &mut patch.provenance.params_snapshot;
    match rendered.matched.as_ref() {
        Some(matched) => params["postprocess"] = matched.provenance(),
        None => {
            if let Some(params) = params.as_object_mut() {
                params.remove("postprocess");
            }
        }
    }
    // A picked finishing replaces the tone alignment a 2.0.0 composite
    // made, as it does when a render is attached with one.
    match rendered.tone.as_ref() {
        Some(tone) => params["tone"] = tone.provenance(),
        None => {
            if let Some(params) = params.as_object_mut() {
                params.remove("tone");
            }
        }
    }
    patch.pixels = rendered.pixels;
    patch.mask = rendered.mask;
    job.complete_display_region(source_idx, &patch, reopened.record.review_state.clone())
        .map_err(|e| e.to_string())?;
    crate::underlay::refresh_dependencies(&mut job, region_id)?;
    crate::library::invalidate_manifest_cache(job_path);

    let after = crate::history::side_now(&chapter_id, &job.project, source_idx, region_id);
    let history = match (before, after) {
        (Some(before), Some(after)) => Some(crate::history::record_region_step(
            job_path,
            &chapter_id,
            crate::history::FINISHING_LABEL,
            region_id,
            before,
            after,
        )?),
        _ => None,
    };
    answer(&job, history)
}

/// The chapter holding `region_id`, and the app's attempt journal.
fn resolve(app: &tauri::AppHandle, region_id: &str) -> Result<(std::path::PathBuf, AttemptJournal), String> {
    let library = Library::for_app(app)?;
    let chapter_id = crate::library::chapter_holding(&library.index()?, region_id)
        .ok_or_else(|| format!("refinish_no_patch: no chapter holds region {region_id}"))?;
    let job_path = library.resolve_chapter(&chapter_id)?;
    Ok((job_path, super::commands::get_app_journal(app)?))
}

/// A preview of region `region_id` finished by every finishing there is, from
/// its saved cloud answer, and the finishing it has now. A region that
/// cannot be finished again answers with why, and no previews.
#[tauri::command]
pub async fn region_finishing_previews(
    app: tauri::AppHandle,
    region_id: String,
) -> Result<FinishingPreviews, String> {
    crate::library::blocking(move || {
        let (job_path, journal) = resolve(&app, &region_id)?;
        previews_at(&journal, &job_path, &region_id)
    })
    .await
}

/// Finish region `region_id` again by `finishing` (grey or not), one undo
/// step. Refused with a `refinish_*` code when it cannot be
/// ([`Unavailable`]), or with `page_busy` while a run is on its page.
#[tauri::command]
pub async fn set_region_finishing(
    app: tauri::AppHandle,
    region_id: String,
    finishing: Finishing,
    grayscale: bool,
) -> Result<FinishingChange, String> {
    crate::library::blocking(move || {
        let (job_path, journal) = resolve(&app, &region_id)?;
        apply_at(&journal, &job_path, &region_id, Choice { finishing, grayscale })
    })
    .await
}
