//! Bounded, ordered input for an edit. The raw source digest remains a separate
//! identity; this digest describes the pixels a renderer actually reads.

use cleaner_core::composite::composite_region;
use cleaner_core::constants::{
    ANNULUS_WIDTH, DETECTOR_INPUT, EDIT_MARGIN, MASK_GROWTH_STEP, MASK_GROWTH_STEPS,
    MIN_MASK_THICKNESS,
};
use cleaner_core::engines::model::plan;
use cleaner_core::engines::render::{PreparedRender, Preprocessing};
use cleaner_core::fit::{self, EdgeMap};
use cleaner_core::image::Raster;
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::patch::{Engine, Patch};
use cleaner_core::project::Job;
use cleaner_core::project::PatchRecord;
use cleaner_core::strip::Strip;
use cleaner_core::strip::{DecodeWindow, EdgePad, JoinState, Joins};
use sha2::{Digest, Sha256};
use std::borrow::Cow;

#[cfg(test)]
thread_local! {
    static PATCH_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) const READ_MARGIN: i64 = 768;
const MAX_READ_PIXELS: u64 = 4096 * 4096;

pub(crate) struct Input {
    pub image: Raster,
    /// Bounds within `raw`, before translation to the strip or source page.
    pub window: Rect,
    pub digest: String,
    /// Hash of complete visible predecessor records and patch bytes that
    /// intersect the read window, including order and visibility.
    pub predecessors: String,
}

#[derive(Clone, Copy)]
pub(crate) struct StripSpace<'a> {
    pub strip: &'a Strip,
    pub origin: (i64, i64),
}

fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (r > x && bottom > y).then(|| Rect::new(x, y, (r - x) as u32, (bottom - y) as u32))
}

pub(crate) fn window_for(bounds: Rect, width: u32, height: u32) -> Result<Rect, String> {
    if bounds.w == 0 || bounds.h == 0 {
        return Err("empty edit bounds".into());
    }
    let x = bounds.x.saturating_sub(READ_MARGIN).max(0);
    let y = bounds.y.saturating_sub(READ_MARGIN).max(0);
    let right = bounds.right().saturating_add(READ_MARGIN).min(width as i64);
    let bottom = bounds
        .bottom()
        .saturating_add(READ_MARGIN)
        .min(height as i64);
    if right <= x || bottom <= y {
        return Err("edit bounds outside source".into());
    }
    let window = Rect::new(x, y, (right - x) as u32, (bottom - y) as u32);
    if u64::from(window.w) * u64::from(window.h) > MAX_READ_PIXELS {
        return Err("edit context exceeds bounded read budget".into());
    }
    Ok(window)
}

pub(crate) fn read(
    job: &Job,
    source_idx: usize,
    raw: &Raster,
    bounds: Rect,
    below: u32,
    strip: Option<StripSpace<'_>>,
) -> Result<Input, String> {
    let window = window_for(bounds, raw.width, raw.height)?;
    read_exact(job, source_idx, raw, window, below, strip)
}

pub(crate) fn read_exact(
    job: &Job,
    source_idx: usize,
    raw: &Raster,
    window: Rect,
    below: u32,
    strip: Option<StripSpace<'_>>,
) -> Result<Input, String> {
    if window.w == 0
        || window.h == 0
        || window.x < 0
        || window.y < 0
        || window.right() > raw.width as i64
        || window.bottom() > raw.height as i64
        || u64::from(window.w) * u64::from(window.h) > MAX_READ_PIXELS
    {
        return Err("invalid bounded read window".into());
    }
    let mut patches = Vec::new();
    let mut identities = Sha256::new();
    identities.update(b"visible_predecessors_v1\0");
    for record in &job.project.patches {
        if !record.visible || record.order >= below {
            continue;
        }
        let translation = if let Some(space) = strip {
            let Some((_, placed)) = space
                .strip
                .pages()
                .iter()
                .enumerate()
                .find(|(position, _)| {
                    job.project.strip.order.get(*position) == Some(&record.source_idx)
                })
            else {
                continue;
            };
            (
                placed.x_offset - space.origin.0,
                placed.y_offset - space.origin.1,
            )
        } else {
            if record.source_idx != source_idx {
                continue;
            }
            (0, 0)
        };
        let bounds=cleaner_core::project::orientation::display_bbox(&job.project,record);
        let translated = Rect::new(bounds.x+translation.0,bounds.y+translation.1,bounds.w,bounds.h);
        if intersection(translated, window).is_none() {
            continue;
        }
        #[cfg(test)]
        PATCH_LOADS.with(|count| count.set(count.get() + 1));
        let mut patch = job.load_display_patch(record).map_err(|e| e.to_string())?;
        patch.mask.bounds.x += translation.0;
        patch.mask.bounds.y += translation.1;
        patch.ink.bounds.x += translation.0;
        patch.ink.bounds.y += translation.1;
        identities.update(serde_json::to_vec(record).map_err(|e| e.to_string())?);
        identities.update(&patch.mask.bits);
        identities.update(&patch.ink.bits);
        identities.update(&patch.pixels.data);
        patches.push(patch);
    }
    let image = composite_region(raw, &patches, window, Some(below)).map_err(|e| e.to_string())?;
    if image.width != window.w || image.height != window.h {
        return Err("compositor returned an undersized input window".into());
    }
    let mut digest = Sha256::new();
    digest.update(b"edit_input_v1\0");
    let (source_x, source_y) = if let Some(space) = strip {
        let (_, anchor) = space
            .strip
            .pages()
            .iter()
            .enumerate()
            .find(|(position, _)| job.project.strip.order.get(*position) == Some(&source_idx))
            .ok_or("source absent from strip during input digest")?;
        (
            window.x + space.origin.0 - anchor.x_offset,
            window.y + space.origin.1 - anchor.y_offset,
        )
    } else {
        (window.x, window.y)
    };
    digest.update(source_x.to_le_bytes());
    digest.update(source_y.to_le_bytes());
    digest.update(window.w.to_le_bytes());
    digest.update(window.h.to_le_bytes());
    digest.update(serde_json::to_vec(&(image.mode, image.depth)).map_err(|e| e.to_string())?);
    digest.update(&image.data);
    Ok(Input {
        image,
        window,
        digest: format!("{:x}", digest.finalize()),
        predecessors: format!("{:x}", identities.finalize()),
    })
}

fn read_record_footprint(
    job: &Job,
    record: &PatchRecord,
    footprint: Rect,
    strip: &Strip,
    decoded_source_idx: usize,
    decoded_source: &Raster,
) -> Result<Input, String> {
    if job.project.strip.mode != cleaner_core::project::StripMode::Longstrip {
        if record.source_idx != decoded_source_idx {
            return Err("dependency source changed".into());
        }
        return read_exact(
            job,
            record.source_idx,
            decoded_source,
            footprint,
            record.order,
            None,
        );
    }
    let (_, anchor) = strip
        .pages()
        .iter()
        .enumerate()
        .find(|(position, _)| job.project.strip.order.get(*position) == Some(&record.source_idx))
        .ok_or("dependency anchor missing from strip")?;
    let global = Rect::new(
        footprint.x + anchor.x_offset,
        footprint.y + anchor.y_offset,
        footprint.w,
        footprint.h,
    );
    if global.w == 0
        || global.h == 0
        || global.x < 0
        || global.y < 0
        || global.right() > strip.width() as i64
        || global.bottom() > strip.height() as i64
        || u64::from(global.w) * u64::from(global.h) > MAX_READ_PIXELS
    {
        return Err("invalid strip dependency footprint".into());
    }
    let mut joins = Joins::unchecked(strip.joins());
    for join in 0..strip.joins() {
        joins.set(join, JoinState::Verified);
    }
    let rect = strip.clamp(global, &joins);
    let decoded = cleaner_core::strip::read_window_borrowing(
        strip,
        DecodeWindow {
            rect,
            requested: global,
            pad: EdgePad::None,
        },
        |position| {
            let source_idx = *job
                .project
                .strip
                .order
                .get(position)
                .ok_or("dependency strip source missing")?;
            if source_idx == decoded_source_idx {
                return Ok(Cow::Borrowed(decoded_source));
            }
            let path = job
                .source_path(source_idx)
                .ok_or("dependency source file missing")?;
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            job.display_page(source_idx,&bytes)
                .map(Cow::Owned)
                .map_err(|e| e.to_string())
        },
    )?;
    read_exact(
        job,
        record.source_idx,
        &decoded.raster,
        Rect::new(0, 0, rect.w, rect.h),
        record.order,
        Some(StripSpace {
            strip,
            origin: decoded.origin,
        }),
    )
}

/// The proxy scale a fit's reach is sized for in a longstrip, before the
/// page's own size says more. A fit's growth series is given in proxy pixels
/// ([`cleaner_core::detect::Segmentation::proxy_scale`]): the detection window's
/// long side over the detector's input. On a paginated chapter that window is
/// at most the page, so the page answers exactly; a strip's window runs down
/// the strip and can be taller than one page, so it is given room.
const LONGSTRIP_FIT_SCALE_FLOOR: f32 = 4.0;

/// How far past the mask it settled on a fit may have read, at proxy scale
/// one: the thickest candidate of its growth series and the annulus it samples
/// around it. The chosen mask contains the seed, so every candidate it tried
/// lies within this of the mask the patch records.
const FIT_SPAN: u32 = MIN_MASK_THICKNESS + MASK_GROWTH_STEP * MASK_GROWTH_STEPS + ANNULUS_WIDTH;

/// How far past a region a fit reads at the proxy scale it ran at: the
/// thickest candidate of its growth series and the annulus it samples, plus
/// the edit margin. The margin also covers the one pixel a Sobel reads past
/// [`fit_statistics_bounds`], and at scale one the quality check's surround
/// around the applied mask (32 px and a Sobel pixel).
fn fit_margin(scale: f32) -> i64 {
    (FIT_SPAN as f32 * scale).ceil() as i64 + i64::from(EDIT_MARGIN)
}

fn grown(rect: Rect, by: i64) -> Rect {
    Rect::new(
        rect.x - by,
        rect.y - by,
        (i64::from(rect.w) + 2 * by).max(0) as u32,
        (i64::from(rect.h) + 2 * by).max(0) as u32,
    )
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect::new(x, y, (right - x) as u32, (bottom - y) as u32)
}

/// The rectangle the fit's page statistics are measured over: the seed and
/// every candidate mask its growth series can reach. The fit only ever asks
/// about pixels in here, so measuring its edge threshold anywhere wider would
/// make a layer depend on page it never looks at, and an edit far away could
/// change its mask. Clipped to the raster.
pub(crate) fn fit_statistics_bounds(seed: Rect, scale: f32, width: u32, height: u32) -> Rect {
    let span = grown(seed, (FIT_SPAN as f32 * scale).ceil() as i64);
    intersection(span, Rect::new(0, 0, width, height)).unwrap_or(Rect::new(0, 0, width, height))
}

/// What a render wrote through `applied` read besides its fit, by engine:
/// the fill ring and the quality check's surround for every rung; LaMa's
/// tiles, which are [`cleaner_core::engines::model::MODEL_INPUT`] squares
/// **centred on** a region that fits
/// one and laid exactly over one that does not ([`plan`]), so they reach at
/// most `(512 - extent) / 2` past it; and the FLUX crop. `ladder` counts the
/// rungs a ladder may have tried and declined on the way up, whose reads
/// decided that it went on.
fn engine_reads(engine: Engine, applied: Rect, ladder: bool, preprocessing: Preprocessing) -> Rect {
    let mut read = grown(applied, fit_margin(1.0));
    let rung = crate::run::rung(engine);
    if engine == Engine::Lama || (ladder && rung > crate::run::rung(Engine::Lama)) {
        for tile in plan(applied) {
            read = union(read, tile);
        }
    }
    if let Some(crop) = preprocessing.crop_for(applied).filter(|_| matches!(engine, Engine::Flux | Engine::Cloud)) {
        read = union(read, crop);
    }
    read
}

/// Everything a render read, within the `window` it was given: the fit
/// around its `seed` at the proxy `scale`, and what its engine read around the
/// `applied` mask ([`engine_reads`]). Recorded with the layer as its `reach`
/// (with a digest of the pixels in it), so a later edit is tested against
/// what this layer really read and not against its whole bounded window.
pub(crate) fn render_reach(engine: Engine, seed: Rect, scale: f32, applied: Rect, ladder: bool, window: Rect) -> Rect {
    render_reach_as(engine, seed, scale, applied, ladder, window, Preprocessing::CURRENT)
}

fn render_reach_as(engine: Engine, seed: Rect, scale: f32, applied: Rect, ladder: bool,
    window: Rect, preprocessing: Preprocessing) -> Rect {
    if matches!(engine, Engine::Paint | Engine::Clone) {
        return window;
    }
    let read = union(grown(seed, fit_margin(scale)), engine_reads(engine, applied, ladder, preprocessing));
    intersection(read, window).unwrap_or(window)
}

/// The same answer for a layer that recorded no `reach`, from its record:
/// how far past the pixels it wrote a layer's renderer may have read, in that
/// page's pixels.
///
/// **This, and not the bounded read window, decides whether an earlier edit
/// may have changed such a layer's input.** The window ([`READ_MARGIN`] on
/// every side) is what an edit composites so that no renderer can run short
/// of context, and on an ordinary page it is nearly the whole page. Testing a
/// change against it flagged every later layer on a page the moment any
/// earlier one was faded, deleted or cleaned again, which is the "everything
/// needs review" state.
///
/// The seed is not recorded, so the fit is measured from what was written, at
/// the largest proxy scale the page allows, and a ladder is assumed for any
/// layer the cloud did not render. Paint and
/// clone read exactly the window their stroke recorded. Always within the
/// recorded footprint.
fn dependency_reach(record: &PatchRecord, footprint: Rect, page: (u32, u32), longstrip: bool) -> Rect {
    if matches!(record.provenance.engine, Engine::Paint | Engine::Clone) {
        return footprint;
    }
    let floor = if longstrip { LONGSTRIP_FIT_SCALE_FLOOR } else { 1.0 };
    let scale = (page.0.max(page.1) as f32 / DETECTOR_INPUT as f32).max(floor);
    // A cloud render is one engine, never a ladder's last rung.
    let ladder = record.provenance.cloud.is_none();
    let read = union(
        grown(record.bbox, fit_margin(scale)),
        engine_reads(record.provenance.engine, record.bbox, ladder, Preprocessing::CURRENT),
    );
    intersection(read, footprint).unwrap_or(read)
}

/// Called under the job lock after an earlier layer is changed. Existing
/// output remains committed; only the review state changes. Old records have
/// no read provenance and are marked unknown only if what their renderer
/// would have read ([`dependency_reach`]) meets the change.
pub(crate) fn refresh_dependencies(job: &mut Job, changed_id: &str) -> Result<(), String> {
    refresh_dependencies_from(job, changed_id, None)
}

/// [`refresh_dependencies`] for an edit that moved where the changed layer is
/// drawn: `previous` is the page rectangle it was drawn in before the edit.
/// The layer's pixels left that place, so a later layer that read it has
/// changed input even though the layer is no longer there.
pub(crate) fn refresh_dependencies_from(
    job: &mut Job,
    changed_id: &str,
    previous: Option<Rect>,
) -> Result<(), String> {
    let Some(changed) = job
        .project
        .patches
        .iter()
        .find(|p| p.id == changed_id)
        .cloned()
    else {
        return Ok(());
    };
    // Every place this change could have altered what a later layer read:
    // where the patch was made, where it is drawn now, where it was drawn
    // before this edit, and where each earlier revision of it was. A re-run
    // or a restore replaces one mask with another, and the pixels of the old
    // one are gone from under its neighbours as surely as the new ones arrived.
    let drawn = |record: &PatchRecord| [cleaner_core::project::orientation::display_bbox(&job.project,record)];
    let mut touched: Vec<Rect> = drawn(&changed).into_iter().chain(previous).collect();
    for revision in job
        .project
        .legacy_patch_revisions
        .iter()
        .chain(&job.project.text_shape_patch_revisions)
        .filter(|revision| revision.id == changed.id && revision.source_idx == changed.source_idx)
    {
        touched.extend(drawn(revision));
    }
    touched.sort_by_key(|rect| (rect.x, rect.y, rect.w, rect.h));
    touched.dedup();
    let strip = crate::run::strip_of(&job.project);
    let changed_place = strip
        .pages()
        .iter()
        .enumerate()
        .find(|(position, _)| job.project.strip.order.get(*position) == Some(&changed.source_idx))
        .map(|(_, p)| *p);
    let longstrip = job.project.strip.mode == cleaner_core::project::StripMode::Longstrip;
    let later = job
        .project
        .patches
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.visible
                && p.order > changed.order
                && (p.source_idx == changed.source_idx || longstrip)
        })
        .map(|(i, p)| (i, p.clone()))
        .collect::<Vec<_>>();
    if later.is_empty() {
        return Ok(());
    }
    let source = job
        .source_path(changed.source_idx)
        .ok_or("source missing for dependency review")?;
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    let raw = job.display_page(changed.source_idx,&bytes).map_err(|e| e.to_string())?;
    let mut dirty = false;
    let dependency = |state: Option<&str>| {
        matches!(state, Some("review.reason.inputChanged" | "review.reason.inputUnknown"))
    };
    for (index, record) in later {
        // The changed places, in this record's page coordinates.
        let shift = if record.source_idx == changed.source_idx {
            (0, 0)
        } else {
            let Some(from) = changed_place else { continue };
            let Some((_, to)) = strip.pages().iter().enumerate().find(|(position, _)| {
                job.project.strip.order.get(*position) == Some(&record.source_idx)
            }) else {
                continue;
            };
            (from.x_offset - to.x_offset, from.y_offset - to.y_offset)
        };
        let page = (
            job.project.sources[record.source_idx].w,
            job.project.sources[record.source_idx].h,
        );
        let provenance = record.provenance.params_snapshot.get("input_provenance");
        let rect_at = |key: &str| {
            provenance
                .and_then(|p| p.get(key))
                .and_then(|v| serde_json::from_value::<Rect>(v.clone()).ok())
        };
        let digest_at = |key: &str| provenance.and_then(|p| p.get(key)).and_then(|v| v.as_str());
        let footprint = rect_at("read_footprint")
            .unwrap_or_else(|| window_for(record.bbox, page.0, page.1).unwrap_or(record.bbox));
        // What the layer read: recorded with it when it was made, or else
        // worked out from its record.
        let recorded = rect_at("reach").zip(digest_at("reach_sha256"));
        let reach = recorded.map_or_else(|| dependency_reach(&record, footprint, page, longstrip), |(reach, _)| reach);
        let meets = touched.iter().any(|rect| {
            let moved = Rect::new(rect.x + shift.0, rect.y + shift.1, rect.w, rect.h);
            intersection(reach, moved).is_some()
        });
        let flagged = dependency(job.project.patches[index].review_state.as_deref());
        // A layer already flagged is looked at again whether or not this edit
        // reaches it: a flag an older, wider rule raised, or one an undo has
        // since answered, clears once the pixels it read are shown to be the
        // same. It is never cleared on anything less.
        if !meets && !flagged {
            continue;
        }
        // Whether what the layer read is what it reads now: over its reach
        // when it recorded one, over its whole window when that is all it
        // recorded, and unknown otherwise.
        let (rect, expected) = match recorded {
            Some((reach, digest)) => (reach, Some(digest)),
            None => (footprint, digest_at("input_sha256")),
        };
        let same = expected.map(|expected| {
            read_record_footprint(job, &record, rect, &strip, changed.source_idx, &raw)
                .map(|input| input.digest == expected)
        });
        let next = match same {
            Some(Ok(true)) => None,
            Some(Ok(false)) => Some("review.reason.inputChanged".to_owned()),
            // Reached by this edit, and nothing to compare against.
            _ if meets => Some("review.reason.inputUnknown".to_owned()),
            // Not reached, and nothing that shows the flag is wrong: kept.
            _ => continue,
        };
        let current = &mut job.project.patches[index];
        if next.is_none() && !dependency(current.review_state.as_deref()) {
            continue;
        }
        if current.review_state != next {
            if !dependency(current.review_state.as_deref()) && dependency(next.as_deref()) {
                current.provenance.params_snapshot["review_before_input_change"] =
                    serde_json::json!(current.review_state);
            }
            current.review_state = next.or_else(|| {
                current
                    .provenance
                    .params_snapshot
                    .get("review_before_input_change")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            });
            dirty = true;
        }
    }
    if dirty {
        job.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn local_mask(mask: &Mask, window: Rect) -> Mask {
    let mut result = mask.clone();
    result.bounds.x -= window.x;
    result.bounds.y -= window.y;
    result
}

pub(crate) fn page_rect(local: Rect, window: Rect) -> Rect {
    Rect::new(local.x + window.x, local.y + window.y, local.w, local.h)
}

/// A renderer may replicate a true page edge, but it cannot mistake an
/// insufficient composited window edge for the page edge.
pub(crate) fn require_footprint(
    input: &Input,
    footprint: Rect,
    raw: &Raster,
) -> Result<(), String> {
    let page = Rect::new(0, 0, raw.width, raw.height);
    if let Some(needed) = intersection(page_rect(footprint, input.window), page) {
        let covered = intersection(needed, input.window);
        if covered != Some(needed) {
            return Err("model read footprint exceeds composited window".into());
        }
    }
    Ok(())
}

pub(crate) struct CloudInput {
    pub input: Input,
    pub prepared: PreparedRender,
    pub crop_bounds: Rect,
    /// Map coordinates in the decoded strip window back to the anchor source.
    pub source_shift: (i64, i64),
    /// What the render reads ([`render_reach`]), in the same coordinates as
    /// `input.window`, and the digest of the pixels there.
    pub reach: Rect,
    pub reach_digest: String,
}

/// [`prepare_cloud_seed`] for a stored patch: refused when the patch is hidden,
/// and prepared from the hole `preprocessing` takes of it - the lettering the
/// patch removed under V2, so a second render of a cloud patch removes exactly
/// what the first did instead of the first one's write support.
pub(crate) fn prepare_cloud(
    job: &Job,
    source_idx: usize,
    raw: &Raster,
    record: &PatchRecord,
    patch: &Patch,
    preprocessing: Preprocessing,
) -> Result<CloudInput, String> {
    if !record.visible {
        return Err("cloud target is hidden or deleted".into());
    }
    let hole = preprocessing.hole(&patch.mask, &patch.ink);
    prepare_cloud_seed(job, source_idx, raw, record.order, hole, preprocessing)
}

/// The cloud render input of a region, from the hole the model is asked to
/// remove (`seed_mask`, which the preparation takes as it is: a stored hole was
/// fitted when it was stored, and fitting it again would move it) and the
/// place in page order it would take, which is all the preparation reads of a
/// region. A stored detection has no patch yet, and this is how it is
/// prepared (`docs/detect-clean.md` §3). The crop, hint and composite follow
/// `preprocessing`, which the render's recipe names.
pub(crate) fn prepare_cloud_seed(
    job: &Job,
    source_idx: usize,
    raw: &Raster,
    order: u32,
    seed_mask: &Mask,
    preprocessing: Preprocessing,
) -> Result<CloudInput, String> {
    if seed_mask.is_empty() {
        return Err("seed mask is empty".into());
    }
    let strip = crate::run::strip_of(&job.project);
    let strip_read = if job.project.strip.mode == cleaner_core::project::StripMode::Longstrip {
        let (position, anchor) = strip
            .pages()
            .iter()
            .enumerate()
            .find(|(position, _)| job.project.strip.order.get(*position) == Some(&source_idx))
            .ok_or("anchor page is missing from strip")?;
        let global = Rect::new(
            seed_mask.bounds.x + anchor.x_offset,
            seed_mask.bounds.y + anchor.y_offset,
            seed_mask.bounds.w,
            seed_mask.bounds.h,
        );
        let requested = Rect::new(
            global.x.saturating_sub(1024).max(0),
            global.y.saturating_sub(1024).max(0),
            0,
            0,
        );
        let right = global
            .right()
            .saturating_add(1024)
            .min(strip.width() as i64);
        let bottom = global
            .bottom()
            .saturating_add(1024)
            .min(strip.height() as i64);
        if right <= requested.x || bottom <= requested.y {
            return Err("cloud target outside strip".into());
        }
        let requested = Rect::new(
            requested.x,
            requested.y,
            (right - requested.x) as u32,
            (bottom - requested.y) as u32,
        );
        let mut joins = Joins::unchecked(strip.joins());
        for join in 0..strip.joins() {
            joins.set(join, JoinState::Verified);
        }
        let rect = strip.clamp(requested, &joins);
        if u64::from(rect.w) * u64::from(rect.h) > MAX_READ_PIXELS {
            return Err("cloud strip context exceeds bounded read budget".into());
        }
        let window = DecodeWindow {
            rect,
            requested,
            pad: EdgePad::None,
        };
        let decoded = cleaner_core::strip::read_window_borrowing(&strip, window, |at| {
            if at == position {
                return Ok(Cow::Borrowed(raw));
            }
            let other_source = *job
                .project
                .strip
                .order
                .get(at)
                .ok_or_else(|| format!("strip source {at} missing"))?;
            let path = job
                .source_path(other_source)
                .ok_or_else(|| format!("strip source {at} has no file"))?;
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            job.display_page(other_source,&bytes)
                .map(Cow::Owned)
                .map_err(|e| e.to_string())
        })?;
        let source_shift = (
            decoded.origin.0 - anchor.x_offset,
            decoded.origin.1 - anchor.y_offset,
        );
        Some((decoded, source_shift))
    } else {
        None
    };
    let (read_page, source_shift) = strip_read
        .as_ref()
        .map_or((raw, (0, 0)), |(decoded, shift)| (&decoded.raster, *shift));
    let local_bounds = Rect::new(
        seed_mask.bounds.x - source_shift.0,
        seed_mask.bounds.y - source_shift.1,
        seed_mask.bounds.w,
        seed_mask.bounds.h,
    );
    let strip_space = strip_read.as_ref().map(|(decoded, _)| StripSpace {
        strip: &strip,
        origin: decoded.origin,
    });
    let input = read(
        job,
        source_idx,
        read_page,
        local_bounds,
        order,
        strip_space,
    )?;
    let mut seed = seed_mask.clone();
    seed.bounds.x -= source_shift.0;
    seed.bounds.y -= source_shift.1;
    let seed = local_mask(&seed, input.window);
    // The noise floor from the raw page, which no layer edit changes, and the
    // edges only where the fit looks: what an earlier edit can change under
    // this render is then only what [`render_reach`] records.
    let noise = fit::page_noise_sigma(read_page);
    let edges = EdgeMap::sobel_within(
        &input.image,
        fit_statistics_bounds(seed.bounds, 1.0, input.image.width, input.image.height),
    );
    let fitted = fit::fit(&input.image, &seed, 1.0, noise, &edges, true);
    let applied =
        cleaner_core::engines::model::applied_mask(&fitted, input.image.width, input.image.height);
    let prepared = PreparedRender::prepare_as(&input.image, &fitted, preprocessing)
        .map_err(|e| e.to_string())?;
    require_footprint(&input, prepared.crop(), read_page)?;
    let local = render_reach_as(Engine::Cloud, seed.bounds, 1.0, applied.bounds, false,
        Rect::new(0, 0, input.window.w, input.window.h), preprocessing);
    let reach = Rect::new(local.x + input.window.x, local.y + input.window.y, local.w, local.h);
    let reach_digest = read_exact(job, source_idx, read_page, reach, order, strip_space)?.digest;
    let crop_bounds = page_rect(
        prepared.crop(),
        Rect::new(
            input.window.x + source_shift.0,
            input.window.y + source_shift.1,
            0,
            0,
        ),
    );
    Ok(CloudInput {
        input,
        prepared,
        crop_bounds,
        source_shift,
        reach,
        reach_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::image::{encode, BitDepth, ColorMode, Format};
    use cleaner_core::ingest::source_ref;
    use cleaner_core::patch::{Engine, Provenance};
    use cleaner_core::project::{Project, StripMode};

    #[test]
    fn orientation_underlay_reads_the_same_legacy_moved_layer_as_the_preview() {
        for value in 1..=8 {
            let (root,mut job,raw)=dependency_page(&format!("orientation-{value}"), &[("a",30,Engine::Paint,Nothing)]);
            job.project.sources[0].orientation=cleaner_core::image::orientation::Orientation(value);
            job.project.patches[0].provenance.params_snapshot["layer"]=serde_json::json!({"offsetX":7,"offsetY":3,"rotation":90.0,"opacity":70});
            let record=&job.project.patches[0];
            let native=cleaner_core::composite::composite(&raw,&[job.load_patch(record).unwrap()]).unwrap();
            let source=&job.project.sources[0];let expected=source.orientation.raster(&native);let displayed=source.orientation.raster(&raw);
            let bbox=cleaner_core::project::orientation::display_bbox(&job.project,record);
            let got=read_exact(&job,0,&displayed,bbox,1,None).unwrap();
            let crop=cleaner_core::composite::composite_region(&expected,&[],bbox,None).unwrap();
            assert_eq!(got.image.data,crop.data,"orientation {value}");
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn dependency_footprint_clamps_across_narrow_strip_join() {
        let root = std::env::temp_dir().join(format!(
            "mc-underlay-narrow-join-{}", std::process::id()
        ));
        let raws = root.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let gray = |width: u32| Raster {
            width, height: 10, mode: ColorMode::Gray, depth: BitDepth::Eight,
            icc: None, palette: None, trns: None, srgb_intent: None,
            color: Default::default(),
            data: vec![255; width as usize * 10],
        };
        let sources = [gray(10), gray(8)].into_iter().enumerate().map(|(index, raw)| {
            let path = raws.join(format!("{index}.png"));
            let bytes = encode(&raw, Format::Png).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            source_ref(&path, &bytes).unwrap()
        }).collect::<Vec<_>>();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let job = Job::create(&manifest, Project::new(
            manifest.parent().unwrap(), "fixture", StripMode::Longstrip, &sources,
        )).unwrap();
        let bounds = Rect::new(0, 8, 10, 2);
        let mask = Mask::filled(bounds);
        let patch = Patch {
            id: "dependent".into(), mask: mask.clone(), ink: mask,
            pixels: Raster { height: 2, data: vec![255; 20], ..gray(10) },
            order: 0, visible: true,
            provenance: Provenance {
                engine: Engine::Fill, engine_version: "fixture".into(),
                model_sha256: None, execution_provider: "cpu".into(),
                params_snapshot: serde_json::json!({}), mask_sha256: String::new(),
                source_sha256: String::new(), cloud: None, created: 0,
            },
        };
        let record = PatchRecord::of(0, &patch, None);
        let input = read_record_footprint(
            &job, &record, Rect::new(0, 8, 10, 4),
            &crate::run::strip_of(&job.project), 0, &gray(10),
        ).unwrap();
        assert_eq!((input.image.width, input.image.height), (8, 4));
        std::fs::remove_dir_all(root).ok();
    }

    /// What a test layer recorded of its input when it was made.
    #[derive(Clone, Copy, PartialEq)]
    enum Recorded {
        /// Nothing: a layer from before input provenance.
        Nothing,
        /// Its bounded window and the digest of it, as builds before the
        /// recorded reach wrote.
        Window,
        /// That, and what it read of the window with the digest of that.
        Reach,
    }
    use Recorded::{Nothing, Reach, Window};

    /// A 600 x 200 grey page in a fresh job, with one layer per `(id, x,
    /// engine, recorded)` committed in that order, each a 20 px square at
    /// `(x, 90)`. A recorded layer carries the input provenance an edit
    /// writes, taken just before it was committed.
    fn dependency_page(
        name: &str,
        layers: &[(&str, i64, Engine, Recorded)],
    ) -> (std::path::PathBuf, Job, Raster) {
        let root = std::env::temp_dir().join(format!("mc-underlay-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let raw = Raster {
            width: 600, height: 200, mode: ColorMode::Gray, depth: BitDepth::Eight,
            icc: None, palette: None, trns: None, srgb_intent: None,
            color: Default::default(),
            data: vec![40; 600 * 200],
        };
        let path = root.join("page.png");
        let bytes = encode(&raw, Format::Png).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let source = source_ref(&path, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let mut job = Job::create(&manifest, Project::new(
            manifest.parent().unwrap(), "fixture", StripMode::Single, &[source],
        )).unwrap();
        for (order, (id, x, engine, recorded)) in layers.iter().enumerate() {
            let bounds = Rect::new(*x, 90, 20, 20);
            let mut snapshot = serde_json::json!({});
            if *recorded != Nothing {
                let input = read(&job, 0, &raw, bounds, order as u32, None).unwrap();
                snapshot["input_provenance"] = serde_json::json!({
                    "version": 1, "input_sha256": input.digest,
                    "predecessors_sha256": input.predecessors, "read_footprint": input.window,
                });
                if *recorded == Reach {
                    let reach = render_reach(*engine, bounds, 1.0, bounds, false, input.window);
                    let digest = read_exact(&job, 0, &raw, reach, order as u32, None).unwrap().digest;
                    snapshot["input_provenance"]["reach"] = serde_json::json!(reach);
                    snapshot["input_provenance"]["reach_sha256"] = serde_json::json!(digest);
                }
            }
            let mask = Mask::filled(bounds);
            job.complete_region(0, &Patch {
                id: (*id).into(), mask: mask.clone(), ink: mask,
                pixels: Raster { width: 20, height: 20, data: vec![230; 400], ..raw.clone() },
                order: order as u32, visible: true,
                provenance: Provenance {
                    engine: *engine, engine_version: "fixture".into(), model_sha256: None,
                    execution_provider: "cpu".into(), params_snapshot: snapshot,
                    mask_sha256: String::new(), source_sha256: String::new(), cloud: None, created: 0,
                },
            }, None).unwrap();
        }
        (root, job, raw)
    }

    fn review_of<'a>(job: &'a Job, id: &str) -> Option<&'a str> {
        job.project.patches.iter().find(|p| p.id == id).unwrap().review_state.as_deref()
    }

    /// **The "everything needs review" defect.** Every layer's bounded read
    /// window reaches most of an ordinary page, and testing an earlier edit
    /// against it flagged every later layer on the page. Only a layer whose
    /// renderer read near the change is flagged now; the rest keep their
    /// clean state, recorded provenance or not.
    #[test]
    fn an_earlier_edit_flags_only_the_layers_that_read_near_it() {
        let (root, mut job, _) = dependency_page("reach", &[
            ("a", 10, Engine::Fill, Nothing),
            // Beside it, recorded: its input really changes.
            ("near", 40, Engine::Fill, Window),
            // Far off, recorded: inside its read window, outside its reach.
            ("far", 500, Engine::Fill, Window),
            // Far off, never recorded: not guessed at either.
            ("far-old", 520, Engine::Fill, Nothing),
            // Beside it, never recorded: may have changed, and says so.
            ("near-old", 60, Engine::Fill, Nothing),
            // LaMa's one tile is centred on the region: at 200..220 it runs
            // from -46, over the change at 10..30.
            ("lama", 200, Engine::Lama, Window),
            // At 300..320 it runs from 54: the change is outside it, though
            // it is inside a 512 px margin.
            ("lama-far", 300, Engine::Lama, Window),
            // A FLUX crop is a few dozen pixels of context: 190 px is outside.
            // Recorded as this build records it; one that recorded only its
            // window may have come up a ladder through LaMa's tile.
            ("flux", 220, Engine::Flux, Reach),
        ]);
        job.project.patches.iter_mut().find(|p| p.id == "a").unwrap().visible = false;
        job.flush().unwrap();
        refresh_dependencies(&mut job, "a").unwrap();
        assert_eq!(review_of(&job, "near"), Some("review.reason.inputChanged"));
        assert_eq!(review_of(&job, "near-old"), Some("review.reason.inputUnknown"));
        assert_eq!(review_of(&job, "lama"), Some("review.reason.inputChanged"));
        assert_eq!(review_of(&job, "lama-far"), None);
        assert_eq!(review_of(&job, "far"), None);
        assert_eq!(review_of(&job, "far-old"), None);
        assert_eq!(review_of(&job, "flux"), None);

        // Undone, the recorded neighbour reads what it read before and clears;
        // the unrecorded one cannot tell and stays flagged.
        job.project.patches.iter_mut().find(|p| p.id == "a").unwrap().visible = true;
        job.flush().unwrap();
        refresh_dependencies(&mut job, "a").unwrap();
        assert_eq!(review_of(&job, "near"), None);
        assert_eq!(review_of(&job, "lama"), None);
        assert_eq!(review_of(&job, "near-old"), Some("review.reason.inputUnknown"));
        std::fs::remove_dir_all(root).ok();
    }

    /// LaMa reads its tiles and no more: one [`MODEL_INPUT`] square centred on
    /// a region that fits, nothing past a region tiled edge to edge.
    #[test]
    fn a_lama_layer_reaches_its_tiles_not_a_model_input_margin() {
        use cleaner_core::engines::model::MODEL_INPUT;
        let small = Rect::new(300, 90, 20, 20);
        let reach = render_reach(Engine::Lama, small, 1.0, small, false, Rect::new(-2000, -2000, 5000, 5000));
        let side = i64::from(MODEL_INPUT);
        assert_eq!((reach.x, reach.right()), (310 - side / 2, 310 + side / 2), "one tile, centred");
        let wide = Rect::new(0, 0, 1200, 20);
        let reach = render_reach(Engine::Lama, wide, 1.0, wide, false, Rect::new(-2000, -2000, 5000, 5000));
        assert_eq!((reach.x, reach.right()), (-fit_margin(1.0), 1200 + fit_margin(1.0)),
            "tiled edge to edge: only the fit and the quality check read past it");
    }

    #[test]
    fn cloud_reach_uses_the_render_recipes_crop() {
        let seed = Rect::new(500, 500, 20, 20);
        let window = Rect::new(0, 0, 2000, 2000);
        let v1 = render_reach_as(Engine::Cloud, seed, 1.0, seed, false, window, Preprocessing::V1);
        let v2 = render_reach_as(Engine::Cloud, seed, 1.0, seed, false, window, Preprocessing::V2);
        let v1_crop = Preprocessing::V1.crop_for(seed).unwrap();
        let v2_crop = Preprocessing::V2.crop_for(seed).unwrap();
        assert_eq!(v1.right(), v1_crop.right().max(seed.right() + fit_margin(1.0)));
        assert_eq!(v2.right(), v2_crop.right().max(seed.right() + fit_margin(1.0)));
        assert!(v1.right() < v2.right(), "an in-flight 1.0.0 render read less context");
    }

    /// The fit's edge statistics, the Sobel pixel past them and the quality
    /// check's surround all lie inside what a render records it read, so an
    /// edit outside that cannot change the layer.
    #[test]
    fn the_recorded_reach_holds_everything_a_render_measures() {
        let window = Rect::new(0, 0, 4000, 4000);
        for (engine, ladder) in [(Engine::Fill, false), (Engine::Fill, true), (Engine::Lama, false),
            (Engine::Flux, false), (Engine::Cloud, false), (Engine::Flux, true)]
        {
            for scale in [1.0f32, 2.344, 4.0] {
                let seed = Rect::new(1800, 1900, 140, 60);
                // A model rung writes through the lettering, which can sit
                // well inside the seed.
                let applied = Rect::new(1850, 1915, 30, 20);
                let reach = render_reach(engine, seed, scale, applied, ladder, window);
                let stats = grown(fit_statistics_bounds(seed, scale, 4000, 4000), 1);
                let surround = grown(applied, 33);
                for inner in [stats, surround] {
                    assert_eq!(intersection(inner, reach), Some(inner), "{engine:?} at {scale}: {inner:?} not in {reach:?}");
                }
                if engine == Engine::Lama || (ladder && crate::run::rung(engine) > crate::run::rung(Engine::Lama)) {
                    for tile in plan(applied) {
                        assert_eq!(intersection(tile, reach), Some(tile));
                    }
                }
            }
        }
    }

    /// **A digest over what was read, not over the window.** A far edit and
    /// a near one flag a layer that recorded only its window, and undoing the
    /// near one cannot clear it: the far change is still in the window. A
    /// layer that recorded its reach is flagged by the near edit only, and
    /// clears when it is undone, since the pixels it read are the same again.
    #[test]
    fn a_layer_that_recorded_its_reach_clears_when_what_it_read_is_back() {
        let (root, mut job, _) = dependency_page("reach-digest", &[
            ("far-edit", 500, Engine::Fill, Nothing),
            ("near-edit", 10, Engine::Fill, Nothing),
            ("windowed", 40, Engine::Fill, Window),
            ("reached", 40, Engine::Fill, Reach),
        ]);
        let hide = |job: &mut Job, id: &str, visible: bool| {
            job.project.patches.iter_mut().find(|p| p.id == id).unwrap().visible = visible;
            job.flush().unwrap();
            refresh_dependencies(job, id).unwrap();
        };
        hide(&mut job, "far-edit", false);
        assert_eq!(review_of(&job, "reached"), None, "the far edit is outside what it read");
        hide(&mut job, "near-edit", false);
        assert_eq!(review_of(&job, "reached"), Some("review.reason.inputChanged"));
        assert_eq!(review_of(&job, "windowed"), Some("review.reason.inputChanged"));
        hide(&mut job, "near-edit", true);
        assert_eq!(review_of(&job, "reached"), None, "what it read is back");
        assert_eq!(review_of(&job, "windowed"), Some("review.reason.inputChanged"),
            "its window still differs, and nothing narrower was recorded");
        std::fs::remove_dir_all(root).ok();
    }

    /// A flag an older, wider rule raised is looked at again on the next
    /// edit of the page, even one far from the layer: cleared when the layer's
    /// recorded pixels prove its input is what it was, kept otherwise, and
    /// never raised where nothing reached.
    #[test]
    fn an_old_flag_is_checked_again_and_cleared_only_on_proof() {
        let (root, mut job, _) = dependency_page("recheck", &[
            ("elsewhere", 500, Engine::Fill, Nothing),
            ("proved", 40, Engine::Fill, Window),
            ("unprovable", 100, Engine::Fill, Nothing),
            ("still-changed", 160, Engine::Fill, Window),
        ]);
        for id in ["proved", "unprovable", "still-changed"] {
            job.project.patches.iter_mut().find(|p| p.id == id).unwrap().review_state =
                Some("review.reason.inputUnknown".into());
        }
        job.project.patches.iter_mut().find(|p| p.id == "still-changed").unwrap()
            .provenance.params_snapshot["input_provenance"]["input_sha256"] = serde_json::json!("another-input");
        job.flush().unwrap();
        job.project.patches.iter_mut().find(|p| p.id == "elsewhere").unwrap().visible = false;
        job.flush().unwrap();
        refresh_dependencies(&mut job, "elsewhere").unwrap();
        // "proved" read a window the far edit is inside; its digest now
        // differs, so it is not cleared, but it now says what it knows.
        assert_eq!(review_of(&job, "proved"), Some("review.reason.inputChanged"));
        assert_eq!(review_of(&job, "unprovable"), Some("review.reason.inputUnknown"), "no proof either way: kept");
        assert_eq!(review_of(&job, "still-changed"), Some("review.reason.inputChanged"));
        // Put back, the window reads as it did: the recorded layer clears.
        job.project.patches.iter_mut().find(|p| p.id == "elsewhere").unwrap().visible = true;
        job.flush().unwrap();
        refresh_dependencies(&mut job, "elsewhere").unwrap();
        assert_eq!(review_of(&job, "proved"), None);
        assert_eq!(review_of(&job, "unprovable"), Some("review.reason.inputUnknown"));
        assert_eq!(review_of(&job, "still-changed"), Some("review.reason.inputChanged"));
        std::fs::remove_dir_all(root).ok();
    }

    /// A move takes a layer's pixels away from where it was drawn. The layer
    /// that read them there is flagged even though neither the layer's own
    /// box nor where it is drawn now is anywhere near it.
    #[test]
    fn a_layer_moved_away_flags_what_read_it_where_it_was() {
        let (root, mut job, _) = dependency_page("moved", &[
            ("a", 300, Engine::Fill, Nothing),
            ("reader", 40, Engine::Fill, Window),
        ]);
        // Record the reader against nothing under it, then say the layer was
        // drawn beside it and has moved further off: its box and its new
        // place are both out of reach, its old place is not.
        let a = job.project.patches.iter_mut().find(|p| p.id == "a").unwrap();
        a.provenance.params_snapshot["layer"] = serde_json::json!({ "opacity": 100, "offsetX": 200 });
        job.project.patches.iter_mut().find(|p| p.id == "reader").unwrap()
            .provenance.params_snapshot["input_provenance"]["input_sha256"] = serde_json::json!("before-the-move");
        job.flush().unwrap();
        refresh_dependencies(&mut job, "a").unwrap();
        assert_eq!(review_of(&job, "reader"), None, "nothing it read moved, as far as the record says");
        refresh_dependencies_from(&mut job, "a", Some(Rect::new(60, 90, 20, 20))).unwrap();
        assert_eq!(review_of(&job, "reader"), Some("review.reason.inputChanged"));
        std::fs::remove_dir_all(root).ok();
    }

    /// **A copy of a real chapter**, when one is supplied: run with
    /// `MC_DEMO_CHAPTER=<a copied .mtclean beside its .mtclean.d> cargo test
    /// -p manga-cleaner real_chapter_edit_flags -- --ignored --nocapture`.
    /// Manifest and sidecar are copied again into scratch first. On every page
    /// it hides the lowest layer, as a delete does, and reports how many later
    /// layers the old window test would have flagged against how many are
    /// flagged now.
    #[test]
    #[ignore]
    fn real_chapter_edit_flags() {
        let Some(given) = std::env::var_os("MC_DEMO_CHAPTER") else { return };
        let given = std::path::PathBuf::from(given);
        fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
            std::fs::create_dir_all(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let target = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() { copy_tree(&entry.path(), &target) }
                else { std::fs::copy(entry.path(), target).unwrap(); }
            }
        }
        let root = std::env::temp_dir().join(format!("mc-underlay-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let manifest = root.join(given.file_name().unwrap());
        std::fs::copy(&given, &manifest).unwrap();
        let sidecar = given.with_extension("mtclean.d");
        copy_tree(&sidecar, &root.join(sidecar.file_name().unwrap()));
        let mut job = Job::open(&manifest).unwrap();
        let (mut before, mut after, mut later_total) = (0, 0, 0);
        for source_idx in 0..job.project.sources.len() {
            let Some(lowest) = job.project.patches.iter()
                .filter(|p| p.visible && p.source_idx == source_idx)
                .min_by_key(|p| p.order).cloned() else { continue };
            let later: Vec<_> = job.project.patches.iter()
                .filter(|p| p.visible && p.source_idx == source_idx && p.order > lowest.order).cloned().collect();
            let page = &job.project.sources[source_idx];
            let (w, h) = (page.w, page.h);
            let old = later.iter().filter(|record| {
                let footprint = record.provenance.params_snapshot.get("input_provenance")
                    .and_then(|p| p.get("read_footprint"))
                    .and_then(|v| serde_json::from_value::<Rect>(v.clone()).ok())
                    .unwrap_or_else(|| window_for(record.bbox, w, h).unwrap_or(record.bbox));
                intersection(footprint, lowest.bbox).is_some()
            }).count();
            job.project.patches.iter_mut().find(|p| p.id == lowest.id).unwrap().visible = false;
            job.flush().unwrap();
            refresh_dependencies(&mut job, &lowest.id).unwrap();
            let now = later.iter().filter(|record| {
                job.project.patches.iter().find(|p| p.id == record.id).unwrap().review_state.as_deref()
                    .is_some_and(|state| matches!(state, "review.reason.inputChanged" | "review.reason.inputUnknown"))
            }).count();
            println!("page {:02}: {} later layers, old window test {old}, flagged now {now}", source_idx + 1, later.len());
            before += old;
            after += now;
            later_total += later.len();
        }
        println!("total: {later_total} later layers, old window test {before}, flagged now {after}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn insufficient_model_context_is_refused() {
        let raw = Raster {
            width: 100,
            height: 100,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![255; 100 * 100],
        };
        let input = Input {
            image: Raster {
                width: 20,
                height: 20,
                data: vec![255; 20 * 20],
                ..raw.clone()
            },
            window: Rect::new(10, 10, 20, 20),
            digest: String::new(),
            predecessors: String::new(),
        };
        assert!(require_footprint(&input, Rect::new(0, 0, 30, 30), &raw).is_err());
        assert!(window_for(Rect::new(10, 10, 4000, 4000), 6000, 6000).is_err());
    }

    /// Run explicitly with `--ignored --nocapture`; reports measured process
    /// peak as well as the bounded composited buffer on this host.
    #[test]
    #[ignore]
    fn bounded_4000x6000_dense_history_benchmark() {
        for (mode, channels) in [(ColorMode::Gray, 1), (ColorMode::Rgb, 3)] {
            let started = std::time::Instant::now();
            let root = std::env::temp_dir().join(format!("mc-underlay-bench-{}-{channels}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let raw = Raster {
                width: 4000,
                height: 6000,
                mode,
                depth: BitDepth::Eight,
                icc: None,
                palette: None,
                trns: None,
                srgb_intent: None,
                color: Default::default(),
                data: vec![255; 4000 * 6000 * channels],
            };
            let bytes = encode(&raw, Format::Png).unwrap();
            let source_path = root.join("page.png");
            std::fs::write(&source_path, &bytes).unwrap();
            let source = source_ref(&source_path, &bytes).unwrap();
            let manifest = root.join("job/chapter.mtclean");
            std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            let mut job = Job::create(
                &manifest,
                Project::new(
                    manifest.parent().unwrap(),
                    "bench",
                    StripMode::Single,
                    &[source],
                ),
            )
            .unwrap();
            for order in 0..128 {
                let bounds = if order < 96 {
                    Rect::new(1940 + (order % 8) as i64 * 3,
                        2940 + (order % 8) as i64 * 3, 64, 64)
                } else {
                    Rect::new(100 + (order % 8) as i64 * 3, 100, 64, 64)
                };
                let mask = Mask::filled(bounds);
                let patch = Patch {
                    id: format!("p{order}"),
                    mask: mask.clone(),
                    ink: mask,
                    pixels: Raster {
                        width: 64,
                        height: 64,
                        mode,
                        depth: BitDepth::Eight,
                        icc: None,
                        palette: None,
                        trns: None,
                        srgb_intent: None,
                        color: Default::default(),
                        data: vec![order as u8; 64 * 64 * channels],
                    },
                    order,
                    visible: true,
                    provenance: Provenance {
                        engine: Engine::Fill,
                        engine_version: "bench".into(),
                        model_sha256: None,
                        execution_provider: "cpu".into(),
                        params_snapshot: serde_json::json!({}),
                        mask_sha256: String::new(),
                        source_sha256: String::new(),
                        cloud: None,
                        created: 0,
                    },
                };
                job.complete_region(0, &patch, None).unwrap();
            }
            let outside: Vec<_> = job.project.patches.iter()
                .filter(|record| record.order >= 96)
                .map(|record| cleaner_core::project::sidecar_dir(&manifest).join(&record.buffer_ref))
                .collect();
            assert_eq!(outside.len(), 32);
            for path in &outside {
                std::fs::remove_file(path).unwrap();
            }
            PATCH_LOADS.with(|count| count.set(0));
            let read_started = std::time::Instant::now();
            let input = read(&job, 0, &raw, Rect::new(1970, 2970, 80, 80), 128, None).unwrap();
            let loaded = PATCH_LOADS.with(std::cell::Cell::get);
            assert_eq!(loaded, 96);
            let read_ms = read_started.elapsed().as_millis();
            assert!(input.image.width <= 80 + READ_MARGIN as u32 * 2);
            assert!(input.image.height <= 80 + READ_MARGIN as u32 * 2);
            assert!(input.image.data.len() < raw.data.len() / 4);
            // `libc` is a unix-only dependency; the figure is informational.
            #[cfg(unix)]
            let peak = {
                let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
                if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
                    unsafe { usage.assume_init().ru_maxrss }
                } else {
                    0
                }
            };
            #[cfg(not(unix))]
            let peak = 0;
            println!(
                "mode={mode:?} raw={} window={} patches_loaded={} patches_outside_unloaded={} peak_rss={} read_ms={} total_ms={}",
                raw.data.len(),
                input.image.data.len(),
                loaded,
                outside.len(),
                peak,
                read_ms,
                started.elapsed().as_millis()
            );
            std::fs::remove_dir_all(root).ok();
        }
    }
}
