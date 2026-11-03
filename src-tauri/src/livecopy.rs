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
//! copies it reads.
//!
//! Called from [`crate::underlay::refresh_dependencies_of`], which every
//! change to a layer passes through, and after a run commits a page, under the
//! job lock that change holds. A copy that cannot be drawn again here is
//! flagged for review like any other later layer instead ([`redrawn_here`]):
//! on a chapter with a turned page, whose layers are edited through a display
//! reconstruction as the eraser's are, and where its area runs off its page.
//! In a long strip a copy reads its own page only, so a layer of the next page
//! that hangs over the join into the area is not in it.

use cleaner_core::composite::composite_region;
use cleaner_core::mask::Rect;
use cleaner_core::project::orientation::display_bbox;
use cleaner_core::project::{Job, PatchRecord, Project};

/// Whether a stored layer is a live copy.
pub(crate) fn is_live(record: &PatchRecord) -> bool {
    let snapshot = &record.provenance.params_snapshot;
    snapshot.get("mode").and_then(|v| v.as_str()) == Some("region-copy")
        && snapshot.get("live").and_then(|v| v.as_bool()) == Some(true)
}

/// Whether [`redraw`] keeps `record` drawn from its area: a live copy on an
/// upright chapter whose area lies on its own page.
pub(crate) fn redrawn_here(project: &Project, record: &PatchRecord) -> bool {
    let Some(source) = project.sources.get(record.source_idx) else { return false };
    let area = record.bbox;
    project.sources.iter().all(|source| source.orientation.0 == 1)
        && area.x >= 0 && area.y >= 0
        && area.right() <= i64::from(source.w) && area.bottom() <= i64::from(source.h)
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

/// Draw again every live copy on page `source_idx` whose area meets
/// `touched`, the places a change altered, and `changed` itself when it is a
/// live copy (shown again, or put back by an undo, it may hold old pixels).
/// Answers the copies whose pixels changed, as they are stored now. A copy
/// redrawn here alters its own place in turn, so a later live copy reading it
/// is redrawn after it.
pub(crate) fn redraw(job: &mut Job, source_idx: usize, changed: Option<&str>, touched: &[Rect]) -> Result<Vec<PatchRecord>, String> {
    let mut copies: Vec<PatchRecord> = job.project.patches.iter()
        .filter(|record| record.visible && record.source_idx == source_idx && is_live(record))
        .filter(|record| redrawn_here(&job.project, record))
        .cloned()
        .collect();
    copies.sort_by_key(|record| record.order);
    let mut touched = touched.to_vec();
    let reached = |copy: &PatchRecord, touched: &[Rect]| {
        Some(copy.id.as_str()) == changed || touched.iter().any(|rect| overlaps(*rect, copy.bbox))
    };
    if !copies.iter().any(|copy| reached(copy, &touched)) {
        return Ok(Vec::new());
    }
    let source = job.source_path(source_idx).ok_or("source missing for a live copy")?;
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    let page = job.display_page(source_idx, &bytes).map_err(|e| e.to_string())?;
    let mut redrawn = Vec::new();
    for copy in copies {
        if !reached(&copy, &touched) {
            continue;
        }
        let mut patch = job.load_stored_patch(&copy).map_err(|e| e.to_string())?;
        let area = patch.mask.bounds;
        let under = job.project.patches.iter()
            .filter(|record| record.visible && record.id != copy.id && record.source_idx == copy.source_idx)
            .filter(|record| !is_live(record) || record.order < copy.order)
            .filter(|record| overlaps(display_bbox(&job.project, record), area))
            .map(|record| job.load_patch(record))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let pixels = composite_region(&page, &under, area, None).map_err(|e| e.to_string())?;
        if pixels.width != area.w || pixels.height != area.h || pixels.data == patch.pixels.data {
            continue;
        }
        patch.pixels = pixels;
        job.complete_display_region(copy.source_idx, &patch, copy.review_state.clone())
            .map_err(|e| e.to_string())?;
        touched.push(display_bbox(&job.project, &copy));
        if let Some(stored) = job.project.patches.iter().find(|record| record.id == copy.id) {
            redrawn.push(stored.clone());
        }
    }
    Ok(redrawn)
}
