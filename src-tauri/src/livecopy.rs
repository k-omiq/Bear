//! Live copies: region copies that stay linked to the area they were picked
//! from (the clone stamp's Live copy mode).
//!
//! A Region copy freezes the artwork it read when it was made. A live one is
//! the same layer, recorded with `live: true` beside its `region-copy` mode,
//! whose pixels are drawn again from that area whenever a layer there
//! changes: a clean, a paint stroke, an erase, an undo. The area is the
//! copy's stored box, where it was picked, so moving or turning the copy
//! never moves what it reads. Its own mask is kept, so what the eraser took
//! off stays off.
//!
//! **What it reads** is the page and every visible layer over the area but
//! itself and live copies made after it, in their order: a layer made after
//! the copy reaches it too, which is what "stays linked" means, and a live
//! copy reading only older live copies cannot read itself through another.
//! So one pass in order redraws every copy a change reaches, each after the
//! copies it reads. In a long strip that includes a layer of the page above
//! or below hanging over the join into the area, and a change on that page
//! reaches the copy.
//!
//! Called from [`crate::underlay::refresh_dependencies_of`], which every
//! change to a layer passes through, and after a run commits a page, under the
//! job lock that change holds. A copy that cannot be drawn again here is
//! flagged for review like any other later layer instead ([`redrawn_here`]):
//! where its area runs off its page, and where it was saved whole on a page
//! turned since, whose layer style is stated in the page's native frame.
//!
//! Each redraw writes a revision of the copy, as any edit does. The ones no
//! undo step names are dropped when the chapter is next opened
//! ([`prune_revisions`]), when nothing in the interface can still hold one.

use std::collections::HashSet;
use std::path::Path;

use cleaner_core::composite::composite_region;
use cleaner_core::mask::Rect;
use cleaner_core::project::orientation::display_bbox;
use cleaner_core::project::{Job, PatchRecord};
use cleaner_core::strip::Strip;

use crate::underlay::Pages;

/// Whether a stored layer is a live copy.
pub(crate) fn is_live(record: &PatchRecord) -> bool {
    let snapshot = &record.provenance.params_snapshot;
    snapshot.get("mode").and_then(|v| v.as_str()) == Some("region-copy")
        && snapshot.get("live").and_then(|v| v.as_bool()) == Some(true)
}

/// Whether [`redraw`] keeps `record` drawn from its area: a live copy stored
/// in display coordinates ([`Job::stored_in_display_frame`]) whose area lies
/// on its own page.
pub(crate) fn redrawn_here(job: &Job, record: &PatchRecord) -> bool {
    let Some(source) = job.project.sources.get(record.source_idx) else { return false };
    let (width, height) = source.orientation.size(source.w, source.h);
    on_page(record.bbox, width, height) && job.stored_in_display_frame(record)
}

fn on_page(area: Rect, width: u32, height: u32) -> bool {
    area.x >= 0 && area.y >= 0 && area.right() <= i64::from(width) && area.bottom() <= i64::from(height)
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

fn moved(rect: Rect, by: (i64, i64)) -> Rect {
    Rect::new(rect.x + by.0, rect.y + by.1, rect.w, rect.h)
}

/// What to add to a place on page `to` to have it on page `from`: the two
/// pages' distance in the strip, in a long strip, where a layer may hang over
/// a join. `None` for another page of a chapter of single pages, which no
/// layer of `from` reaches.
fn shift(job: &Job, strip: &Strip, from: usize, to: usize) -> Option<(i64, i64)> {
    if from == to {
        return Some((0, 0));
    }
    if job.project.strip.mode != cleaner_core::project::StripMode::Longstrip {
        return None;
    }
    let placed = |source_idx: usize| {
        let position = job.project.strip.order.iter().position(|&index| index == source_idx)?;
        strip.pages().get(position).map(|page| (page.x_offset, page.y_offset))
    };
    let (from, to) = (placed(from)?, placed(to)?);
    Some((to.0 - from.0, to.1 - from.1))
}

/// Draw again every live copy whose area meets `touched`, the places a change
/// on page `source_idx` altered (in that page's pixels), and `changed` itself
/// when it is a live copy (shown again, or put back by an undo, it may hold
/// old pixels). Answers the copies whose pixels changed, as they are stored
/// now. A copy redrawn here alters its own place in turn, so a later live copy
/// reading it is redrawn after it. Each page is decoded once, in `pages`.
pub(crate) fn redraw(
    job: &mut Job,
    pages: &mut Pages,
    source_idx: usize,
    changed: Option<&str>,
    touched: &[Rect],
) -> Result<Vec<PatchRecord>, String> {
    let strip = crate::run::strip_of(&job.project);
    // Each copy with what to add to its places to have them on `source_idx`.
    let mut copies: Vec<(PatchRecord, (i64, i64))> = job.project.patches.iter()
        .filter(|record| record.visible && is_live(record) && redrawn_here(job, record))
        .filter_map(|record| Some((record.clone(), shift(job, &strip, source_idx, record.source_idx)?)))
        .collect();
    copies.sort_by_key(|(record, _)| record.order);
    let mut touched = touched.to_vec();
    let reached = |copy: &PatchRecord, at: (i64, i64), touched: &[Rect]| {
        Some(copy.id.as_str()) == changed || touched.iter().any(|rect| overlaps(*rect, moved(copy.bbox, at)))
    };
    let mut redrawn = Vec::new();
    for (copy, at) in copies {
        if !reached(&copy, at, &touched) {
            continue;
        }
        let mut patch = job.load_display_stored_patch(&copy).map_err(|e| e.to_string())?;
        let area = patch.mask.bounds;
        let page = pages.get(job, copy.source_idx)?;
        if !on_page(area, page.width, page.height) {
            continue;
        }
        // The layers under the area, from this page and, in a long strip,
        // hanging over a join from the next one, on this page's pixels.
        let mut under = Vec::new();
        for record in &job.project.patches {
            if !record.visible || record.id == copy.id || (is_live(record) && record.order > copy.order) {
                continue;
            }
            let Some(by) = shift(job, &strip, copy.source_idx, record.source_idx) else { continue };
            if !overlaps(moved(display_bbox(&job.project, record), by), area) {
                continue;
            }
            let mut layer = job.load_display_patch(record).map_err(|e| e.to_string())?;
            for mask in [&mut layer.mask, &mut layer.ink] {
                mask.bounds = moved(mask.bounds, by);
            }
            under.push(layer);
        }
        let pixels = composite_region(page, &under, area, None).map_err(|e| e.to_string())?;
        if pixels.width != area.w || pixels.height != area.h || pixels.data == patch.pixels.data {
            continue;
        }
        patch.pixels = pixels;
        job.complete_display_region(copy.source_idx, &patch, copy.review_state.clone())
            .map_err(|e| e.to_string())?;
        touched.push(moved(display_bbox(&job.project, &copy), at));
        if let Some(stored) = job.project.patches.iter().find(|record| record.id == copy.id) {
            redrawn.push(stored.clone());
        }
    }
    Ok(redrawn)
}

/// Drop the revisions of live copies that nothing can go back to: not the
/// layer as it stands, and named by no undo or redo step of the chapter's
/// journal. Redraws write one per change under the area, which an undo never
/// asks for (an undo of the change draws the copy again instead), so a busy
/// page would otherwise grow its revision folder for good.
///
/// Run when the chapter is opened, under its lock: the interface then holds
/// no layer read before, so no step it is about to write can name a revision
/// dropped here. A revision another row still names, by its files, keeps them.
pub(crate) fn prune_revisions(job_path: &Path) -> Result<(), String> {
    let mut job = Job::open(job_path).map_err(|e| e.to_string())?;
    let live: HashSet<String> = job.project.patches.iter()
        .filter(|record| is_live(record))
        .map(|record| record.id.clone())
        .collect();
    if live.is_empty() {
        return Ok(());
    }
    let journal = serde_json::to_value(crate::history::load(job_path)).map_err(|e| e.to_string())?;
    let mut named = HashSet::new();
    named_revisions(&journal, &mut named);
    let current: HashSet<(String, Option<String>)> = job.project.patches.iter()
        .map(|record| (record.id.clone(), record.legacy_revision_id()))
        .collect();
    let (dropped, kept): (Vec<PatchRecord>, Vec<PatchRecord>) = std::mem::take(&mut job.project.legacy_patch_revisions)
        .into_iter()
        .partition(|row| {
            let revision = row.legacy_revision_id();
            live.contains(&row.id)
                && revision.as_ref().is_some_and(|revision| !named.contains(revision))
                && !current.contains(&(row.id.clone(), revision))
        });
    job.project.legacy_patch_revisions = kept;
    if dropped.is_empty() {
        return Ok(());
    }
    job.flush().map_err(|e| e.to_string())?;
    let still: HashSet<String> = job.project.patches.iter()
        .chain(&job.project.legacy_patch_revisions)
        .chain(&job.project.text_shape_patch_revisions)
        .chain(&job.project.withdrawn_patches)
        .flat_map(|row| [row.mask_ref.clone(), row.buffer_ref.clone(), row.ink_ref()])
        .collect();
    for row in dropped {
        for reference in [row.mask_ref.clone(), row.buffer_ref.clone(), row.ink_ref()] {
            if reference.starts_with("patch-revisions/") && !still.contains(&reference) {
                let _ = std::fs::remove_file(job.sidecar().join(&reference));
            }
        }
    }
    Ok(())
}

/// Every `legacyPatchRevision` a journal names, at any depth: a step's
/// sides, and the parts of a step of several layers.
fn named_revisions(value: &serde_json::Value, named: &mut HashSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value) {
                    ("legacyPatchRevision", serde_json::Value::String(revision)) => {
                        named.insert(revision.clone());
                    }
                    _ => named_revisions(value, named),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| named_revisions(item, named)),
        _ => {}
    }
}
