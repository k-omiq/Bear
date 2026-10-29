//! A model's result finished again: one region's answer composited by
//! another finishing.
//!
//! The finishing is the client's own step after a model answers
//! ([`cleaner_core::engines::postprocess`]), so the finishing of a region
//! already cleaned can be changed after the fact, from either of two answers:
//!
//! - **A cloud result's saved answer.** A cloud clean keeps the model's raw
//!   answer with its attempt (`journal.rs`, `result.png`). The region is
//!   prepared again from the page under it, held to the input the attempt
//!   sent ([`super::service::check_prepared_input`], the attach's own
//!   checks), and the answer is composited again: no request and no GPU.
//! - **This computer's model, asked again.** A LaMa or local FLUX result
//!   keeps no answer on disk. Its region is prepared again the same way, from
//!   the page under it as it is now, and rendered again by the model that
//!   made it, finished by the method picked ([`LocalRender`]). The model's
//!   session keeps its last answers by their input
//!   ([`cleaner_core::engines::model::AnswerMemo`]), so the previews of every
//!   finishing cost one model run and the pick none, while it stays loaded.
//!
//! What cannot be finished again is said as an [`Unavailable`] reason, which
//! the editor's window shows in place of its previews.

use std::path::Path;

use cleaner_core::cloud_decode::DecodedResultCrop;
use cleaner_core::engines::postprocess::{Choice, Finishing, Report};
use cleaner_core::engines::render::{Preprocessing, ToneReport};
use cleaner_core::fit::Fitted;
use cleaner_core::image::Raster;
use cleaner_core::ingest::sha256_hex;
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::patch::{Engine, Patch};
use cleaner_core::project::{Job, PatchRecord};
use serde::Serialize;

use crate::inference::consent::{encode_rgb8_png, load_cloud_region, CloudRegionKind};
use crate::inference::journal::{AttemptJournal, AttemptPhase, JournalError};
use crate::library::{ApiRegion, Library};

/// The long side of a preview, in pixels: enough to tell the finishings
/// apart in the panel's grid, small enough that all of them cross the
/// seam as one answer.
pub const PREVIEW_LONG_SIDE: u32 = 256;

/// How much context a local result's preview shows round its mask, as a
/// share of the mask's long side, and at least [`PREVIEW_CONTEXT_MIN`] px.
const PREVIEW_CONTEXT: f64 = 0.15;
const PREVIEW_CONTEXT_MIN: i64 = 24;

/// This computer's models, as a local result is finished again with them:
/// [`crate::region::LocalModels`], or a stand-in in the tests.
pub(crate) trait LocalRender {
    /// The model a FLUX render on this computer runs now.
    fn flux_model(&self) -> &str;
    /// `fitted` on `page` rendered by `engine` and finished by `finishing`,
    /// or the `decline.reason.*` key the model refused with.
    fn render(&mut self, engine: Engine, page: &Raster, fitted: &Fitted, finishing: Choice)
        -> Result<crate::run::Made, &'static str>;
}

impl LocalRender for crate::region::LocalModels {
    fn flux_model(&self) -> &str {
        crate::region::LocalModels::flux_model(self)
    }

    fn render(&mut self, engine: Engine, page: &Raster, fitted: &Fitted, finishing: Choice)
        -> Result<crate::run::Made, &'static str> {
        crate::region::LocalModels::render(self, engine, page, fitted, finishing)
    }
}

/// Why a region's finishing cannot be changed. The window says which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Unavailable {
    /// Not a model's result: a fill, a paint or clone stroke, a Patch or
    /// Gradient layer, or a text-shaped result. There is no answer to finish,
    /// so the region is cleaned again instead.
    NotCloud,
    /// The saved answer is gone, or cannot be read.
    NoResult,
    /// The page, or a layer under the region, changed since the answer came
    /// back: it would be composited onto a page it did not answer.
    PageChanged,
    /// Rendered under the first cloud preparation (1.0.0), whose region is
    /// the patch's own mask and moves when the patch is written, or under one
    /// this build does not know; or a local result whose region can no longer
    /// be prepared.
    Unsupported,
    /// A FLUX result from this computer whose model is no longer the one
    /// set: asked again, another model would answer. Cleaned again instead.
    ModelChanged,
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
            Unavailable::ModelChanged => "refinish_model_changed: another local FLUX model is set now",
        }
        .to_owned()
    }
}

/// A result prepared again for its finishing to be changed: the patch as it
/// stands, the preparation of its region, and where its answer comes from.
pub(crate) struct Reopened {
    source_idx: usize,
    record: PatchRecord,
    patch: Patch,
    cloud: crate::underlay::CloudInput,
    answer: Answer,
    /// The finishing the patch's pixels were made by, from its record
    /// (`params_snapshot["postprocess"]`); `None` for a render finished by
    /// its version's own composite.
    current: Option<Choice>,
    /// Whether a colour answer is made grey: the current finishing's, or the
    /// attempt's when the patch records none.
    grayscale: bool,
}

/// Where a reopened result's answer comes from.
enum Answer {
    /// The cloud's, kept with its attempt.
    Cloud(DecodedResultCrop),
    /// This computer's model, LaMa or FLUX, asked again ([`LocalRender`]).
    Local(Engine),
}

/// A result finished by one method, in the coordinates it was rendered in:
/// what a patch is written from.
struct Finished {
    mask: Mask,
    pixels: Raster,
    matched: Option<Report>,
    tone: Option<ToneReport>,
}

/// The finishing a patch's record names, if it names one.
pub(crate) fn recorded_choice(params: &serde_json::Value) -> Option<Choice> {
    let postprocess = params.get("postprocess")?;
    let finishing = serde_json::from_value::<Finishing>(postprocess.get("finishing")?.clone()).ok()?;
    let grayscale = postprocess.get("grayscale").and_then(serde_json::Value::as_bool).unwrap_or(false);
    Some(Choice { finishing, grayscale })
}

/// Prepare region `region_id` of `job` again with its answer, or say why it
/// cannot be. The caller holds the job's lock. An error is a failure to read,
/// or an attempt another command is using; a refusal is `Ok(Err)`.
/// `flux_model` is the local FLUX model set now.
pub(crate) fn reopen(
    journal: &AttemptJournal,
    job: &Job,
    region_id: &str,
    flux_model: &str,
) -> Result<Result<Reopened, Unavailable>, String> {
    let record = job
        .project
        .patches
        .iter()
        .find(|row| row.id == region_id && row.visible)
        .ok_or_else(|| format!("refinish_no_patch: region {region_id} has no cleaned result"))?;
    let Some(attempt_id) = record.provenance.cloud.as_ref().and_then(|cloud| cloud.attempt_id.clone()) else {
        return reopen_local(job, record, flux_model);
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
        answer: Answer::Cloud(answer),
        current,
        grayscale,
    }))
}

/// [`reopen`] for a result no cloud attempt made: one LaMa or FLUX made on
/// this computer, prepared again from the page under it as it is now. Asked
/// again, the model answers that page; a page changed since is no refusal
/// here, as a re-run of the region is none.
fn reopen_local(job: &Job, record: &PatchRecord, flux_model: &str) -> Result<Result<Reopened, Unavailable>, String> {
    let engine = record.provenance.engine;
    if !matches!(engine, Engine::Lama | Engine::Flux)
        || record.provenance.cloud.is_some()
        || record.geometry_policy == cleaner_core::text_shape::GeometryPolicy::TextShape
    {
        return Ok(Err(Unavailable::NotCloud));
    }
    if engine == Engine::Flux
        && record.provenance.params_snapshot.get("flux_model").and_then(serde_json::Value::as_str) != Some(flux_model)
    {
        return Ok(Err(Unavailable::ModelChanged));
    }
    let Some(source_path) = job.source_path(record.source_idx) else {
        return Ok(Err(Unavailable::PageChanged));
    };
    let source = std::fs::read(source_path).map_err(|e| e.to_string())?;
    let source_sha256 = sha256_hex(&source);
    let region = match load_cloud_region(job, &record.id, &source_sha256) {
        Ok(region) => region,
        Err(crate::inference::consent::ConsentError::RenderPrepareFailed(_)) => return Ok(Err(Unavailable::NotCloud)),
        Err(error) => return Err(error.to_string()),
    };
    let page = job.display_page(record.source_idx, &source).map_err(|e| e.to_string())?;
    // The hole the current preparation takes of the patch, the lettering it
    // removed, as an edit's re-run of it reads it.
    let Ok(cloud) = region.prepare(job, &page, Preprocessing::CURRENT) else {
        return Ok(Err(Unavailable::Unsupported));
    };
    let CloudRegionKind::Patch(stored) = region.kind else {
        return Ok(Err(Unavailable::NoResult));
    };
    let (record, patch) = *stored;
    let current = recorded_choice(&record.provenance.params_snapshot);
    Ok(Ok(Reopened {
        source_idx: record.source_idx,
        record,
        patch,
        cloud,
        answer: Answer::Local(engine),
        current,
        grayscale: current.is_some_and(|choice| choice.grayscale),
    }))
}

impl Reopened {
    /// Whether the answer is asked of a model on this computer: the
    /// previews and the pick then run it, and wait for a speed measurement.
    fn is_local(&self) -> bool {
        matches!(self.answer, Answer::Local(_))
    }

    /// The answer finished by `choice`, in the coordinates of the window
    /// the region was prepared in (`cloud.input.image`).
    fn finished(&self, choice: Choice, models: &mut dyn LocalRender) -> Result<Finished, String> {
        match &self.answer {
            Answer::Cloud(answer) => {
                let mut prepared = self.cloud.prepared.clone();
                prepared.set_finishing(Some(choice));
                let rendered = prepared.composite(&answer.as_generated_crop()).map_err(|e| e.to_string())?;
                Ok(Finished { mask: rendered.mask, pixels: rendered.pixels, matched: rendered.matched, tone: rendered.tone })
            }
            Answer::Local(engine) => {
                let made = models
                    .render(*engine, &self.cloud.input.image, &self.cloud.fitted, choice)
                    .map_err(|reason| format!("refinish_declined: {reason}"))?;
                Ok(Finished { mask: made.mask, pixels: made.pixels, matched: made.matched, tone: None })
            }
        }
    }

    /// The answer finished by `choice`, in page coordinates: what the patch
    /// would hold.
    fn rendered(&self, choice: Choice, models: &mut dyn LocalRender) -> Result<Finished, String> {
        let mut finished = self.finished(choice, models)?;
        finished.mask.bounds.x += self.cloud.input.window.x + self.cloud.source_shift.0;
        finished.mask.bounds.y += self.cloud.input.window.y + self.cloud.source_shift.1;
        Ok(finished)
    }

    /// The answer finished by `choice` in the page round it, as a PNG no
    /// longer than [`PREVIEW_LONG_SIDE`] on either side: for a cloud result
    /// the crop the model was shown, for a local one the patch with a margin
    /// of the page ([`PREVIEW_CONTEXT`]).
    fn preview(&self, choice: Choice, models: &mut dyn LocalRender) -> Result<Vec<u8>, String> {
        let Answer::Cloud(answer) = &self.answer else {
            let finished = self.finished(choice, models)?;
            let (w, h, rgb) = in_context(&self.cloud.input.image, &finished);
            return encode_preview(w, h, &rgb);
        };
        let mut prepared = self.cloud.prepared.clone();
        prepared.set_finishing(Some(choice));
        let (_, after) = prepared.preview_rgb8(&answer.as_generated_crop()).map_err(|e| e.to_string())?;
        let crop = prepared.crop();
        if after.len() != crop.w as usize * crop.h as usize * 3 {
            return Err("preview size does not match its crop".into());
        }
        encode_preview(crop.w, crop.h, &after)
    }

    /// One preview per finishing, in [`Finishing::ALL`]'s order, at this
    /// result's grey choice. A cloud answer is composited side by side: a
    /// large crop takes a second or more for each. A local one is rendered
    /// one after another, by the one model session there is.
    fn previews(&self, models: &mut dyn LocalRender) -> Vec<(Finishing, Result<Vec<u8>, String>)> {
        let grayscale = self.grayscale;
        if self.is_local() {
            return Finishing::ALL
                .iter()
                .map(|&finishing| (finishing, self.preview(Choice { finishing, grayscale }, models)))
                .collect();
        }
        std::thread::scope(|scope| {
            let workers: Vec<_> = Finishing::ALL
                .iter()
                .map(|&finishing| {
                    // A cloud preview never asks a model: a stand-in that
                    // answers nothing is all each thread needs.
                    (finishing, scope.spawn(move || self.preview(Choice { finishing, grayscale }, &mut NoModels)))
                })
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

/// No model at all: what a cloud result's previews are made with, which
/// never ask one.
pub(crate) struct NoModels;

impl LocalRender for NoModels {
    fn flux_model(&self) -> &str {
        ""
    }

    fn render(&mut self, _: Engine, _: &Raster, _: &Fitted, _: Choice) -> Result<crate::run::Made, &'static str> {
        Err("decline.reason.rungUnavailable")
    }
}

/// `finished` in `page` with a margin of it round the mask, as 8-bit RGB:
/// the patch's pixels through its mask, the page's everywhere else.
fn in_context(page: &Raster, finished: &Finished) -> (u32, u32, Vec<u8>) {
    let bounds = finished.mask.bounds;
    let margin = ((f64::from(bounds.w.max(bounds.h)) * PREVIEW_CONTEXT).round() as i64).max(PREVIEW_CONTEXT_MIN);
    let x0 = (bounds.x - margin).max(0);
    let y0 = (bounds.y - margin).max(0);
    let x1 = (bounds.right() + margin).min(i64::from(page.width));
    let y1 = (bounds.bottom() + margin).min(i64::from(page.height));
    let view = Rect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32);
    let mut rgb = Vec::with_capacity(view.w as usize * view.h as usize * 3);
    for py in view.y..view.bottom() {
        for px in view.x..view.right() {
            let pixel = if finished.mask.contains(px, py) {
                finished.pixels.rgb8_pixel((px - bounds.x) as u32, (py - bounds.y) as u32)
            } else {
                page.rgb8_pixel(px as u32, py as u32)
            };
            rgb.extend_from_slice(&pixel);
        }
    }
    (view.w, view.h, rgb)
}

/// A `w` by `h` RGB8 picture as a PNG no longer than [`PREVIEW_LONG_SIDE`]
/// on either side.
fn encode_preview(w: u32, h: u32, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let (wu, hu) = (w as usize, h as usize);
    if wu == 0 || hu == 0 || rgb.len() != wu * hu * 3 {
        return Err("preview size does not match its picture".into());
    }
    let scale = f64::from(PREVIEW_LONG_SIDE) / f64::from(w.max(h));
    if scale >= 1.0 {
        return encode_rgb8_png(w, h, rgb).map_err(|e| e.to_string());
    }
    // An area average per channel: a screentone shrunk by sampling would
    // show moire that no finishing made.
    use cleaner_core::engines::grid::{resize_area, Grid};
    let nw = ((wu as f64 * scale).round() as usize).max(1);
    let nh = ((hu as f64 * scale).round() as usize).max(1);
    let mut small = vec![0u8; nw * nh * 3];
    for channel in 0..3 {
        let grid = Grid { w: wu, h: hu, data: rgb.iter().skip(channel).step_by(3).map(|&v| f64::from(v)).collect() };
        for (i, v) in resize_area(&grid, nw, nh).data.into_iter().enumerate() {
            small[i * 3 + channel] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    encode_rgb8_png(nw as u32, nh as u32, &small).map_err(|e| e.to_string())
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
pub(crate) fn previews_at(
    journal: &AttemptJournal,
    job_path: &Path,
    region_id: &str,
    models: &mut dyn LocalRender,
) -> Result<FinishingPreviews, String> {
    // Read under the lock, composited after it: nothing is written, and a
    // long render must not hold the chapter.
    let reopened = {
        let _lock = crate::run::lock_job(job_path)?;
        let job = Job::open(job_path).map_err(|e| e.to_string())?;
        reopen(journal, &job, region_id, models.flux_model())?
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
    let _work = reopened.is_local().then(local_work).transpose()?;
    use base64::Engine as _;
    let previews = reopened
        .previews(models)
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

/// A model on this computer is about to run: refused while a speed
/// measurement holds the processor or the GPU (`measurement::begin_work`),
/// as an edit's model rung is.
fn local_work() -> Result<crate::measurement::Work, String> {
    crate::measurement::begin_work().ok_or_else(|| "refinish_busy: a speed measurement is running, try again".to_owned())
}

/// [`set_region_finishing`] on a resolved chapter: the patch's pixels and
/// mask replaced by the answer finished by `choice`, its post-processing
/// record with them, and one undo step, all under the page's lock. Every
/// other part of the patch - its layer settings, order, review state and
/// provenance - is kept. Asked for the finishing it has, nothing is written.
/// A local result's model runs under that lock, from its session's kept
/// answer when the previews just asked it the same.
pub(crate) fn apply_at(
    journal: &AttemptJournal,
    job_path: &Path,
    region_id: &str,
    choice: Choice,
    models: &mut dyn LocalRender,
) -> Result<FinishingChange, String> {
    let chapter_id = super::service::chapter_id_of(job_path);
    let _lock = match crate::region::page_index_in(&chapter_id, region_id) {
        Some(page_index) => crate::run::lock_page(job_path, &chapter_id, page_index)?,
        None => crate::run::lock_job(job_path)?,
    };
    let mut job = Job::open(job_path).map_err(|e| e.to_string())?;
    let reopened = reopen(journal, &job, region_id, models.flux_model())?.map_err(Unavailable::refusal)?;
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
    let _work = reopened.is_local().then(local_work).transpose()?;
    let rendered = reopened.rendered(choice, models)?;
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
    // A picked finishing replaces the tone alignment a 2.0.0 composite or a
    // LaMa tile made, as it does when a render is attached with one.
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
/// its saved cloud answer or its local model asked again, and the finishing
/// it has now. A region that cannot be finished again answers with why, and
/// no previews.
#[tauri::command]
pub async fn region_finishing_previews(
    app: tauri::AppHandle,
    region_id: String,
) -> Result<FinishingPreviews, String> {
    crate::library::blocking(move || {
        let (job_path, journal) = resolve(&app, &region_id)?;
        previews_at(&journal, &job_path, &region_id, &mut crate::region::LocalModels::open(&app))
    })
    .await
}

/// Finish region `region_id` again by `finishing` (grey or not), one undo
/// step. Refused with a `refinish_*` code when it cannot be
/// ([`Unavailable`]; `refinish_declined` when a local model will not render
/// it), or with `page_busy` while a run is on its page.
#[tauri::command]
pub async fn set_region_finishing(
    app: tauri::AppHandle,
    region_id: String,
    finishing: Finishing,
    grayscale: bool,
) -> Result<FinishingChange, String> {
    crate::library::blocking(move || {
        let (job_path, journal) = resolve(&app, &region_id)?;
        apply_at(&journal, &job_path, &region_id, Choice { finishing, grayscale }, &mut crate::region::LocalModels::open(&app))
    })
    .await
}
