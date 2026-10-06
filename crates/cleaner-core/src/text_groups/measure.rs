//! Offline measurement over real pages: whole-page against cloud-tiled SAM on
//! the same decoded pixels, region/request counts before and after grouping,
//! and our one-crop-per-group schedule against Koharu-style component
//! packing ([`crop_schedules`]). Every report names the exact models behind
//! each grouping ([`describe_models`]). Manual: needs the ONNX Runtime,
//! SAM-TS-L graphs, the full RT-DETR graph and a directory of pages.
//!
//! ```text
//! RT_RUNTIME=.../libonnxruntime.dylib SAM_DIR=.../sam-ts-l RT_FULL_GRAPH=.../detector.onnx \
//! GROUPING_PAGES=.../pages GROUPING_CACHE=.../cache GROUPING_REPORT=.../report.json \
//! [GROUPING_ONLY=01,04] \
//! cargo test -p cleaner-core --release --lib text_groups::measure -- --ignored --nocapture
//! ```
//!
//! Every model answer is cached per page, so a rerun opens no model.
use super::*;
use crate::image::Raster;
use std::path::Path;

/// Cleaning groups whose glyph centres lie in two or more sure bubble boxes
/// (the smallest box holding each centre): one job over two balloons, unless
/// the boxes are lobes of one.
fn spanning_bubbles(grouping: &Grouping) -> usize {
    let bubbles: Vec<Rect> = grouping
        .boxes
        .iter()
        .filter(|b| b.kind == BoxKind::Bubble && b.score >= BUBBLE_SCORE)
        .map(|b| b.rect)
        .collect();
    grouping
        .cleaning()
        .filter(|group| {
            let inside: std::collections::BTreeSet<usize> = group
                .labels
                .iter()
                .filter_map(|&label| {
                    let (cx, cy) = centre(&grouping.components[label as usize - 1].bounds);
                    bubbles
                        .iter()
                        .enumerate()
                        .filter(|(_, rect)| rect.contains(cx, cy))
                        .min_by_key(|(k, rect)| (area(rect), *k))
                        .map(|(k, _)| k)
                })
                .collect();
            inside.len() > 1
        })
        .count()
}

/// Groups to be cleaned (mask lettering or ink estimate) whose lettering
/// islands have centres in two or more sure bubble boxes: the estimate's ink
/// counts here, which [`spanning_bubbles`] does not see.
fn spanning_lettering(grouping: &Grouping) -> usize {
    let bubbles: Vec<Rect> = grouping
        .boxes
        .iter()
        .filter(|b| b.kind == BoxKind::Bubble && b.score >= BUBBLE_SCORE)
        .map(|b| b.rect)
        .collect();
    grouping
        .groups
        .iter()
        .filter(|group| group.disposition == Disposition::Clean)
        .filter(|group| {
            let lettering = grouping.lettering(group);
            if lettering.is_empty() {
                return false;
            }
            let islands = label(lettering.bounds.w, lettering.bounds.h, &lettering.bits);
            let inside: std::collections::BTreeSet<usize> = islands
                .components
                .iter()
                .filter_map(|(bounds, _)| {
                    let (cx, cy) = centre(bounds);
                    let (cx, cy) = (cx + lettering.bounds.x, cy + lettering.bounds.y);
                    bubbles
                        .iter()
                        .enumerate()
                        .filter(|(_, rect)| rect.contains(cx, cy))
                        .min_by_key(|(k, rect)| (area(rect), *k))
                        .map(|(k, _)| k)
                })
                .collect();
            inside.len() > 1
        })
        .count()
}

/// Components named by more than one group, and components above the speck
/// size in no group at all: both are contract violations, so both should be 0.
fn listed_twice_and_lost(grouping: &Grouping) -> (usize, usize) {
    let mut named = std::collections::BTreeMap::<&str, usize>::new();
    for group in &grouping.groups {
        for id in &group.component_ids {
            *named.entry(id.as_str()).or_default() += 1;
        }
    }
    let twice = named.values().filter(|&&n| n > 1).count();
    let lost = grouping
        .components
        .iter()
        .filter(|c| c.pixels >= SPECK_PIXELS && !named.contains_key(c.id.as_str()))
        .count();
    (twice, lost)
}

fn crop(page: &Raster, rect: crate::cloud_analysis_wire::TileRect) -> Raster {
    let mut out = page.rows(rect.y, rect.y + rect.height);
    let samples = page.mode.samples();
    let mut cropped = Raster { width: rect.width, data: Vec::new(), ..out.clone() };
    cropped.data = vec![0; cropped.stride() * rect.height as usize];
    for y in 0..rect.height {
        for x in 0..rect.width {
            for c in 0..samples {
                let value = out.sample(rect.x + x, y, c);
                cropped.set_sample(x, y, c, value);
            }
        }
    }
    out.data.clear();
    cropped
}

/// The pre-grouping production path: one box per SAM component, merged only
/// where boxes overlap, then Ogkalu boxes adopted where uncovered.
fn legacy_regions(page: &Raster, mask: &[u8], rt: &[BalloonBox]) -> (usize, usize) {
    let labeled = label(page.width, page.height, mask);
    let boxes: Vec<DetBox> = labeled
        .components
        .iter()
        .map(|(rect, _)| DetBox { rect: *rect, confidence: 1.0, language: DetectedLanguage::Japanese })
        .collect();
    let segmentation = crate::detect::Segmentation {
        width: page.width,
        height: page.height,
        levels: mask.to_vec(),
        fit: crate::detect::Letterbox::fit(page.width, page.height),
    };
    let regions = crate::detect::build_regions_separated(boxes.clone(), page.width, page.height, |a, b| {
        crate::balloon::merge_crosses_a_balloon(page, &segmentation, a, b)
    });
    let median = crate::detect::median_box_area(&boxes);
    let adopted = legacy_adopted(page, &regions, rt, median);
    let flagged = regions.iter().chain(adopted.iter()).filter(|r| r.flagged_large).count();
    (regions.len() + adopted.len(), flagged)
}

/// Ogkalu text boxes nothing covered, strongest first, as the pre-grouping run
/// adopted them: centre in no region, overlap at or under the merge share.
fn legacy_adopted(page: &Raster, regions: &[Region], rt: &[BalloonBox], median: i64) -> Vec<Region> {
    let mut candidates: Vec<&BalloonBox> =
        rt.iter().filter(|b| b.class != BalloonClass::Bubble && b.score >= 0.5).collect();
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.rect.y.cmp(&b.rect.y)).then(a.rect.x.cmp(&b.rect.x)));
    let mut adopted: Vec<Region> = Vec::new();
    for b in candidates {
        let (cx, cy) = (b.rect.x + b.rect.w as i64 / 2, b.rect.y + b.rect.h as i64 / 2);
        if regions
            .iter()
            .chain(&adopted)
            .any(|r| r.masking.contains(cx, cy) || crate::detect::overlaps_enough(&b.rect, &r.masking))
        {
            continue;
        }
        adopted.push(Region::from_box(b.rect, b.score, DetectedLanguage::Japanese, page.width, page.height, median));
    }
    adopted
}

/// SAM-TS-L's `mask` over the `sam` spatial input and Ogkalu Full's `rt`
/// boxes over `rt_spatial`'s, both run on this machine (a tile plan is the
/// cloud's, emulated here). The grouping counts group without the page's
/// pixels, as they always have; the crop comparison passes `page` for the ink
/// estimate production takes under a text box the mask missed (`run.rs`
/// `group_segment`).
fn grouped(
    page: &Raster,
    with_page: bool,
    mask: &[u8],
    rt: &[BalloonBox],
    (sam, rt_spatial): (SpatialInput, SpatialInput),
    seams: Vec<Seam>,
) -> Grouping {
    let mut inputs = Inputs::new(page.width, page.height, Some(mask));
    inputs.page = with_page.then_some(page);
    inputs.rt = rt;
    inputs.models = vec![
        ModelUse::new(EvidenceModel::SamTsL, Execution::Local, sam),
        ModelUse::new(EvidenceModel::OgkaluFull, Execution::Local, rt_spatial),
    ];
    inputs.seams = seams;
    group(&inputs).unwrap()
}

fn summary(page: &Raster, grouping: &Grouping, mask: &[u8]) -> serde_json::Value {
    let median = grouping.block_median_area();
    let clean: Vec<&TextGroup> = grouping.cleaning().collect();
    let flagged = clean.iter().filter(|g| grouping.region(g, median).flagged_large).count();
    let lettering: u64 = clean.iter().map(|g| u64::from(g.lettering_pixels)).sum();
    let mut areas: Vec<i64> = clean.iter().map(|g| area(&g.bounds)).collect();
    areas.sort_unstable();
    let (listed_twice, lost) = listed_twice_and_lost(grouping);
    serde_json::json!({
        "spanningTwoBubbleBoxes": spanning_bubbles(grouping),
        "spanningTwoBubblesByLettering": spanning_lettering(grouping),
        "componentsListedTwice": listed_twice,
        "componentsLost": lost,
        "components": grouping.components.len(),
        "maskPixels": mask.iter().filter(|v| **v != 0).count(),
        "cleaningGroups": clean.len(),
        "detectorOnly": grouping.detector_only().count(),
        "candidates": grouping.candidates().count(),
        "specks": grouping.components.iter().filter(|c| c.group.is_none()).count(),
        "flaggedLarge": flagged,
        "flaggedReasons": grouping.groups.iter().filter(|g| !g.reasons.is_empty()).count(),
        "crossesBalloon": grouping.groups.iter().filter(|g| g.reasons.contains(&ReviewReason::CrossesBalloon)).count(),
        "blockMedianArea": median,
        "blockAreas": areas,
        "letteringPixelsInCleaningGroups": lettering,
        "splitParts": grouping.groups.iter().filter(|g| g.split.is_some()).count(),
        "candidateSizes": grouping.candidates().map(|g| (g.lettering_pixels, g.component_ids.len(), g.bounds.x, g.bounds.y, g.bounds.w, g.bounds.h))
            .collect::<Vec<_>>(),
        "pageHeight": page.height,
    })
}

/// The 1.0.0 cloud plan: non-overlapping tiles of at most 1024.
fn old_tiles(width: u32, height: u32) -> Vec<crate::cloud_analysis_wire::TileRect> {
    let mut tiles = Vec::new();
    for y in (0..height).step_by(1024) {
        for x in (0..width).step_by(1024) {
            tiles.push(crate::cloud_analysis_wire::TileRect { x, y, width: 1024.min(width - x), height: 1024.min(height - y) });
        }
    }
    tiles
}

fn class_code(class: BalloonClass) -> u8 {
    match class {
        BalloonClass::Bubble => 0,
        BalloonClass::TextInBubble => 1,
        BalloonClass::TextFree => 2,
    }
}

fn class_of(code: u8) -> BalloonClass {
    [BalloonClass::Bubble, BalloonClass::TextInBubble, BalloonClass::TextFree][code as usize]
}

type BoxRow = (i64, i64, u32, u32, u8, f32);

fn rows(boxes: &[BalloonBox]) -> Vec<BoxRow> {
    boxes.iter().map(|b| (b.rect.x, b.rect.y, b.rect.w, b.rect.h, class_code(b.class), b.score)).collect()
}

fn unrow(rows: &[BoxRow]) -> Vec<BalloonBox> {
    rows.iter().map(|&(x, y, w, h, c, score)| BalloonBox { rect: Rect::new(x, y, w, h), class: class_of(c), score }).collect()
}

/// How many of `a` have a same-class box in `b` with IoU at least 0.5.
fn matched(a: &[BalloonBox], b: &[BalloonBox]) -> usize {
    a.iter()
        .filter(|x| {
            b.iter().any(|y| {
                let inter = intersection(&x.rect, &y.rect);
                x.class == y.class && inter * 2 >= area(&x.rect) + area(&y.rect) - inter
            })
        })
        .count()
}

/// Mask agreement against the whole-page reference, overall and within 16 px
/// of the lines where a plan cuts the evidence.
fn coverage(width: u32, reference: &[u8], other: &[u8], xs: &[u32], ys: &[u32]) -> serde_json::Value {
    let near = |x: usize, y: usize| {
        xs.iter().any(|&s| (x as i64 - s as i64).abs() < 16) || ys.iter().any(|&s| (y as i64 - s as i64).abs() < 16)
    };
    let (mut both, mut only_ref, mut only_other, mut seam_diff, mut seam_px) = (0u64, 0u64, 0u64, 0u64, 0u64);
    for (i, (a, b)) in reference.iter().zip(other).enumerate() {
        let (a, b) = (*a != 0, *b != 0);
        let (x, y) = (i % width as usize, i / width as usize);
        match (a, b) {
            (true, true) => both += 1,
            (true, false) => only_ref += 1,
            (false, true) => only_other += 1,
            _ => {}
        }
        if (a || b) && near(x, y) {
            seam_px += 1;
            seam_diff += u64::from(a != b);
        }
    }
    serde_json::json!({
        "both": both, "onlyWhole": only_ref, "onlyTiled": only_other,
        "iou": both as f64 / (both + only_ref + only_other).max(1) as f64,
        "maskPixels": both + only_other, "wholeMaskPixels": both + only_ref,
        "nearSeamPixels": seam_px, "nearSeamDisagreement": seam_diff,
    })
}

/* ------------------------------------------------------------------ */
/* Section 7.2: our crop schedule against Koharu's packing            */
/* ------------------------------------------------------------------ */

/// Koharu's packed core bound and crop context per side
/// (`koharu-pipeline/src/stages/inpainting.rs` `TILE_SIZE` and
/// `TILE_CONTEXT`, commit c697b31).
const KOHARU_CORE: u32 = 512;
const KOHARU_CONTEXT: i64 = 128;

/// One render request of a schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CropJob {
    /// The 1-based component labels whose pixels this job writes.
    members: Vec<u32>,
    /// Where it writes them: a Koharu core (or grid cell), or one group's
    /// lettering bounds.
    core: Rect,
    /// What the model is shown.
    crop: Rect,
}

/// Koharu's `inpaint_tiles` (`stages/inpainting.rs:619`) over `labeled`,
/// measurement only. Components are taken in label order (row-major scan). One
/// no larger than [`KOHARU_CORE`] on either side joins the existing core whose
/// bounding box it grows least (ties to the earliest core) while the union
/// still fits, and otherwise opens a core. A larger one is cut into a grid of
/// [`KOHARU_CORE`] cells from its top-left corner, one job per cell holding any
/// of its pixels, cropped around the pixels that cell holds. Every crop adds
/// [`KOHARU_CONTEXT`] a side, clipped to the page; jobs are ordered by core
/// top, left, bottom, right. Koharu's uniform-balloon flat fill, which runs
/// before this, is left out: strict cloud-only cleaning keeps it disabled.
fn koharu_pack(width: u32, height: u32, labeled: &Labeled) -> Vec<CropJob> {
    let context = |r: &Rect| grown_by(r, KOHARU_CONTEXT, width, height);
    let fits = |r: &Rect| r.w <= KOHARU_CORE && r.h <= KOHARU_CORE;
    let mut bounded: Vec<CropJob> = Vec::new();
    let mut split: Vec<CropJob> = Vec::new();
    for (index, (bounds, _)) in labeled.components.iter().enumerate() {
        let label = index as u32 + 1;
        if fits(bounds) {
            let best = bounded
                .iter()
                .enumerate()
                .map(|(k, job)| (k, hull(&job.core, bounds), area(&job.core)))
                .filter(|(_, joined, _)| fits(joined))
                .min_by_key(|(k, joined, before)| (area(joined) - before, *k));
            match best {
                Some((k, joined, _)) => {
                    let job = &mut bounded[k];
                    job.members.push(label);
                    job.core = joined;
                    job.crop = context(&joined);
                }
                None => bounded.push(CropJob { members: vec![label], core: *bounds, crop: context(bounds) }),
            }
            continue;
        }
        let step = i64::from(KOHARU_CORE);
        let mut top = bounds.y;
        while top < bounds.bottom() {
            let bottom = (top + step).min(bounds.bottom());
            let mut left = bounds.x;
            while left < bounds.right() {
                let right = (left + step).min(bounds.right());
                let cell = Rect::new(left, top, (right - left) as u32, (bottom - top) as u32);
                if let Some(held) = label_bounds_in(labeled, width, label, &cell) {
                    split.push(CropJob { members: vec![label], core: cell, crop: context(&held) });
                }
                left = right;
            }
            top = bottom;
        }
    }
    bounded.append(&mut split);
    bounded.sort_by_key(|job| (job.core.y, job.core.x, job.core.bottom(), job.core.right()));
    bounded
}

/// The bounds of `label`'s pixels within `within`, if it has any there.
fn label_bounds_in(labeled: &Labeled, width: u32, label: u32, within: &Rect) -> Option<Rect> {
    let (mut left, mut top, mut right, mut bottom) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for y in within.y..within.bottom() {
        for x in within.x..within.right() {
            if labeled.labels[y as usize * width as usize + x as usize] == label {
                (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x + 1), bottom.max(y + 1));
            }
        }
    }
    (right > left).then(|| Rect::new(left, top, (right - left) as u32, (bottom - top) as u32))
}

/// What both schedules must clean on one raster: every cleaning group's exact
/// lettering (its mask pixels, and its ink estimate where it has one),
/// labeled 8-connected as Koharu labels its mask.
struct Lettering<'a> {
    width: u32,
    height: u32,
    groups: Vec<&'a TextGroup>,
    letters: Vec<Mask>,
    labeled: Labeled,
    /// Per component, the groups (indices into `groups`) holding its pixels.
    owners: Vec<BTreeSet<usize>>,
    /// Per component, the smallest sure bubble box holding its centre, as
    /// [`spanning_bubbles`] places glyphs.
    balloon: Vec<Option<usize>>,
}

/// Every set pixel of `m`, row by row.
fn mask_pixels(m: &Mask) -> impl Iterator<Item = (i64, i64)> + '_ {
    (m.bounds.y..m.bounds.bottom())
        .flat_map(move |y| (m.bounds.x..m.bounds.right()).map(move |x| (x, y)))
        .filter(move |&(x, y)| m.contains(x, y))
}

fn lettering_of_cleaning(grouping: &Grouping) -> Lettering<'_> {
    let (width, height) = (grouping.width, grouping.height);
    let groups: Vec<&TextGroup> = grouping.groups.iter().filter(|g| g.disposition == Disposition::Clean).collect();
    let letters: Vec<Mask> = groups.iter().map(|g| grouping.lettering(g)).collect();
    let mut union = vec![0u8; width as usize * height as usize];
    for m in &letters {
        for (x, y) in mask_pixels(m) {
            union[y as usize * width as usize + x as usize] = 255;
        }
    }
    let labeled = label(width, height, &union);
    let mut owners = vec![BTreeSet::new(); labeled.components.len()];
    for (k, m) in letters.iter().enumerate() {
        for (x, y) in mask_pixels(m) {
            owners[labeled.labels[y as usize * width as usize + x as usize] as usize - 1].insert(k);
        }
    }
    let bubbles: Vec<Rect> = grouping
        .boxes
        .iter()
        .filter(|b| b.kind == BoxKind::Bubble && b.score >= BUBBLE_SCORE)
        .map(|b| b.rect)
        .collect();
    let balloon = labeled
        .components
        .iter()
        .map(|(bounds, _)| {
            let (cx, cy) = centre(bounds);
            bubbles
                .iter()
                .enumerate()
                .filter(|(_, rect)| rect.contains(cx, cy))
                .min_by_key(|(k, rect)| (area(rect), *k))
                .map(|(k, _)| k)
        })
        .collect();
    Lettering { width, height, groups, letters, labeled, owners, balloon }
}

/// Our schedule: one job per cleaning group, in reading order, writing the
/// components its lettering holds. The hole is the group's lettering bounds
/// (the seed its fitted ink grows from; a text box with no lettering at all
/// is its anchor, as [`Grouping::detector_only_region`] takes it), cropped
/// as the cloud path crops: [`Preprocessing::crop_of_hole`] under
/// [`Preprocessing::CURRENT`], the hole grown by the isolation radius with
/// [`crate::engines::render::CONTEXT_WIDE`] a side where the service takes
/// it, snapped to the latent stride and edge-replicated past the page.
fn our_jobs(letters: &Lettering) -> Vec<CropJob> {
    use crate::engines::render::Preprocessing;
    letters
        .groups
        .iter()
        .zip(&letters.letters)
        .enumerate()
        .map(|(k, (group, lettering))| {
            let hole = if lettering.is_empty() { group.anchor.unwrap_or(group.bounds) } else { lettering.bounds };
            let crop = Preprocessing::CURRENT
                .crop_of_hole(hole, letters.width, letters.height)
                .expect("a page crop fits a u32");
            let members = (0..letters.owners.len())
                .filter(|&c| letters.owners[c].contains(&k))
                .map(|c| c as u32 + 1)
                .collect();
            CropJob { members, core: hole, crop }
        })
        .collect()
}

/// One schedule's counts over `letters`. Crop pixels are what is sent (ours
/// may run past the page, edge-replicated); `OnPage` clips them to it.
/// A component "in" a crop is one whose bounds centre the crop holds.
fn schedule_counts(letters: &Lettering, jobs: &[CropJob]) -> serde_json::Value {
    let (width, height) = (letters.width, letters.height);
    let labels = &letters.labeled.labels;
    let components = letters.labeled.components.len();
    let mut writers: Vec<Vec<usize>> = vec![Vec::new(); components];
    for (j, job) in jobs.iter().enumerate() {
        for &member in &job.members {
            writers[member as usize - 1].push(j);
        }
    }
    // Every lettering pixel must be written by exactly one job: a member of
    // it, inside its core. Per job, the bounds of what it writes.
    let (mut uncovered, mut twice) = (BTreeSet::new(), BTreeSet::new());
    let mut written: Vec<Option<Rect>> = vec![None; jobs.len()];
    for (i, &l) in labels.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let (x, y) = ((i % width as usize) as i64, (i / width as usize) as i64);
        let mut n = 0;
        for &j in &writers[l as usize - 1] {
            if jobs[j].core.contains(x, y) {
                n += 1;
                let pixel = Rect::new(x, y, 1, 1);
                written[j] = Some(written[j].map_or(pixel, |r| hull(&r, &pixel)));
            }
        }
        match n {
            0 => uncovered.insert(l),
            1 => false,
            _ => twice.insert(l),
        };
    }
    let writes_pixel_of = |j: usize, x: i64, y: i64| {
        let l = labels[y as usize * width as usize + x as usize];
        l != 0 && jobs[j].core.contains(x, y) && jobs[j].members.contains(&l)
    };
    let groups_of = |cs: &mut dyn Iterator<Item = usize>| cs.flat_map(|c| letters.owners[c].iter().copied()).collect::<BTreeSet<_>>();
    let balloons_of = |cs: &mut dyn Iterator<Item = usize>| cs.filter_map(|c| letters.balloon[c]).collect::<BTreeSet<_>>();
    let in_crop = |job: &CropJob| -> Vec<usize> {
        (0..components)
            .filter(|&c| {
                let (cx, cy) = centre(&letters.labeled.components[c].0);
                job.crop.contains(cx, cy)
            })
            .collect()
    };
    let members = |job: &CropJob| job.members.iter().map(|&l| l as usize - 1).collect::<Vec<_>>();
    let count = |test: &dyn Fn(usize, &CropJob) -> bool| jobs.iter().enumerate().filter(|(j, job)| test(*j, job)).count();
    let on_page = |r: &Rect| clipped(*r, width, height);
    serde_json::json!({
        "jobs": jobs.len(),
        "jobsWritingNoLettering": count(&|_, job| job.members.is_empty()),
        "cropPixels": jobs.iter().map(|job| area(&job.crop)).sum::<i64>(),
        "cropPixelsOnPage": jobs.iter().map(|job| area(&on_page(&job.crop))).sum::<i64>(),
        "corePixels": jobs.iter().map(|job| area(&job.core)).sum::<i64>(),
        "maxCropSide": jobs.iter().map(|job| job.crop.w.max(job.crop.h)).max().unwrap_or(0),
        "cropCoversTwoBalloons": count(&|_, job| balloons_of(&mut in_crop(job).into_iter()).len() > 1),
        "writesTwoBalloons": count(&|_, job| balloons_of(&mut members(job).into_iter()).len() > 1),
        "mixesGroups": count(&|_, job| groups_of(&mut members(job).into_iter()).len() > 1),
        "cropCoversTwoGroups": count(&|_, job| groups_of(&mut in_crop(job).into_iter()).len() > 1),
        // A crop that shows lettering another job writes: that job's result
        // is this one's context, so running them in parallel or in either
        // order gives the two different inputs.
        "cropSeesAnotherJobsLettering": count(&|a, job| {
            (0..jobs.len()).filter(|&b| b != a).any(|b| {
                let Some(target) = written[b] else { return false };
                let (x0, y0) = (job.crop.x.max(target.x), job.crop.y.max(target.y));
                let (x1, y1) = (job.crop.right().min(target.right()), job.crop.bottom().min(target.bottom()));
                (y0..y1).any(|y| (x0..x1).any(|x| writes_pixel_of(b, x, y)))
            })
        }),
        "componentsUncovered": uncovered.len(),
        "componentsWrittenTwice": twice.len(),
        "componentsSplitAcrossJobs": writers.iter().filter(|w| w.len() > 1).count(),
    })
}

/// Section 7.2's comparison on one grouping: ours against Koharu-style
/// packing of the same lettering, and Koharu packing the whole pixel `mask`
/// as it would (candidates and specks included, which we hold untouched).
fn crop_schedules(grouping: &Grouping, mask: &[u8]) -> serde_json::Value {
    let letters = lettering_of_cleaning(grouping);
    let ours = our_jobs(&letters);
    let koharu = koharu_pack(letters.width, letters.height, &letters.labeled);
    let whole = label(grouping.width, grouping.height, mask);
    let whole_jobs = koharu_pack(grouping.width, grouping.height, &whole);
    serde_json::json!({
        "cleaningGroups": letters.groups.len(),
        "components": letters.labeled.components.len(),
        "letteringPixels": letters.labeled.components.iter().map(|(_, n)| u64::from(*n)).sum::<u64>(),
        "ours": schedule_counts(&letters, &ours),
        "koharu": schedule_counts(&letters, &koharu),
        "koharuWholeMask": {
            "components": whole.components.len(),
            "jobs": whole_jobs.len(),
            "cropPixels": whole_jobs.iter().map(|job| area(&job.crop)).sum::<i64>(),
            "corePixels": whole_jobs.iter().map(|job| area(&job.core)).sum::<i64>(),
        },
    })
}

/// Per-page schedule counts summed over a run: numbers add, `max*` fields
/// take the largest, nested objects merge by key.
fn summed(into: &mut serde_json::Value, page: &serde_json::Value) {
    match (into, page) {
        (serde_json::Value::Object(total), serde_json::Value::Object(page)) => {
            for (key, value) in page {
                match total.get_mut(key) {
                    Some(slot) if key.starts_with("max") => {
                        if value.as_u64() > slot.as_u64() {
                            *slot = value.clone();
                        }
                    }
                    Some(slot) => summed(slot, value),
                    None => {
                        total.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (total, page) => {
            if let (Some(a), Some(b)) = (total.as_i64(), page.as_i64()) {
                *total = serde_json::json!(a + b);
            }
        }
    }
}

#[test]
#[ignore = "manual: ONNX Runtime, SAM-TS-L graphs, full RT-DETR graph and page directory required"]
fn sam_whole_versus_tiled_and_grouping_counts() {
    let runtime = std::env::var("RT_RUNTIME").unwrap_or_default();
    if !runtime.is_empty() {
        crate::runtime::load(Path::new(&runtime)).unwrap();
    }
    let sam_dir = std::env::var("SAM_DIR").unwrap_or_default();
    let rt_graph = std::env::var("RT_FULL_GRAPH").unwrap_or_default();
    let pages = std::env::var("GROUPING_PAGES").expect("GROUPING_PAGES");
    let report_path = std::env::var("GROUPING_REPORT").expect("GROUPING_REPORT");
    let only = std::env::var("GROUPING_ONLY").ok();
    // Models open on first use, so a fully cached run opens none.
    let mut rt_model: Option<crate::rt_regions::FullRegions> = None;
    let mut sam: Option<crate::sam_ts::SamTsSession> = None;
    let mut rt = |page: &Raster, whole: bool| -> Vec<BalloonBox> {
        let model = rt_model.get_or_insert_with(|| crate::rt_regions::FullRegions::open_cpu(Path::new(&rt_graph)).unwrap().0);
        if whole { model.detect_whole(page).unwrap() } else { model.detect_halves(page).unwrap() }
    };
    let mut infer = |page: &Raster| -> Vec<u8> {
        let session = sam.get_or_insert_with(|| {
            crate::sam_ts::SamTsSession::open_on(Path::new(&sam_dir), crate::accel::Accelerator::Cpu,
                &crate::sam_ts::Cancellation::default()).unwrap()
        });
        session.infer(page, &crate::sam_ts::Cancellation::default()).unwrap().mask
    };
    let mut names: Vec<_> = std::fs::read_dir(&pages).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    let mut report = Vec::new();
    for path in names.into_iter().filter(|p| p.extension().is_some_and(|e| e == "png")) {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        if only.as_ref().is_some_and(|list| !list.split(',').any(|s| s == stem)) {
            continue;
        }
        let page = crate::image::decode(&std::fs::read(&path).unwrap()).unwrap();
        let (width, height) = (page.width, page.height);
        let cache = std::env::var("GROUPING_CACHE").ok().map(std::path::PathBuf::from).expect("GROUPING_CACHE");
        std::fs::create_dir_all(&cache).unwrap();
        let file = |name: &str| cache.join(format!("{stem}.{name}"));
        let read = |name: &str| std::fs::read(file(name)).ok();

        // Local reference: whole-page SAM and RT-DETR on two halves.
        let local_rt = match read("rt.json") {
            Some(bytes) => unrow(&serde_json::from_slice::<Vec<BoxRow>>(&bytes).unwrap()),
            None => {
                let found = rt(&page, false);
                std::fs::write(file("rt.json"), serde_json::to_vec(&rows(&found)).unwrap()).unwrap();
                found
            }
        };
        let whole = read("whole.bin").unwrap_or_else(|| {
            let mask = infer(&page);
            std::fs::write(file("whole.bin"), &mask).unwrap();
            mask
        });

        // 1.0.0: non-overlapping tiles, pasted whole; boxes concatenated.
        let old = old_tiles(width, height);
        let old_mask = read("tiled.bin").unwrap_or_else(|| {
            let mut stitched = vec![0u8; whole.len()];
            for tile in &old {
                let mask = infer(&crop(&page, *tile));
                crate::cloud_tiles::stitch_mask(&mut stitched, width, *tile, *tile, &mask);
            }
            std::fs::write(file("tiled.bin"), &stitched).unwrap();
            stitched
        });
        let old_rt = match read("rt-oldtiles.json") {
            Some(bytes) => unrow(&serde_json::from_slice::<Vec<BoxRow>>(&bytes).unwrap()),
            None => {
                let mut found = Vec::new();
                for tile in &old {
                    for mut b in rt(&crop(&page, *tile), true) {
                        b.rect = Rect::new(b.rect.x + i64::from(tile.x), b.rect.y + i64::from(tile.y), b.rect.w, b.rect.h);
                        found.push(b);
                    }
                }
                std::fs::write(file("rt-oldtiles.json"), serde_json::to_vec(&rows(&found)).unwrap()).unwrap();
                found
            }
        };

        // 1.1.0: overlapping tiles stitched by core; boxes merged.
        let plan = crate::cloud_tiles::planned(width, height).unwrap();
        let new_mask = read("overlap.bin").unwrap_or_else(|| {
            let mut stitched = vec![0u8; whole.len()];
            for p in &plan {
                let mask = infer(&crop(&page, p.tile));
                crate::cloud_tiles::stitch_mask(&mut stitched, width, p.tile, p.core, &mask);
            }
            std::fs::write(file("overlap.bin"), &stitched).unwrap();
            stitched
        });
        let per_tile: Vec<Vec<BoxRow>> = match read("rt-tiles.json") {
            Some(bytes) => serde_json::from_slice(&bytes).unwrap(),
            None => {
                let found: Vec<Vec<BoxRow>> = plan.iter().map(|p| rows(&rt(&crop(&page, p.tile), true))).collect();
                std::fs::write(file("rt-tiles.json"), serde_json::to_vec(&found).unwrap()).unwrap();
                found
            }
        };
        let tile_boxes: Vec<crate::cloud_tiles::TileBox> = plan
            .iter()
            .zip(&per_tile)
            .flat_map(|(p, found)| {
                unrow(found).into_iter().map(move |mut b| {
                    b.rect = Rect::new(b.rect.x + i64::from(p.tile.x), b.rect.y + i64::from(p.tile.y), b.rect.w, b.rect.h);
                    crate::cloud_tiles::TileBox { tile: p.tile, core: p.core, found: b }
                })
            })
            .collect();
        let cloud_rt = crate::cloud_tiles::merge_tile_boxes(width, height, &tile_boxes);

        let (old_xs, old_ys): (Vec<u32>, Vec<u32>) =
            ((1..).map(|k| k * 1024).take_while(|&v| v < width).collect(), (1..).map(|k| k * 1024).take_while(|&v| v < height).collect());
        let (new_xs, new_ys) = crate::cloud_tiles::core_boundaries(width, height);
        let seams = |xs: &[u32], ys: &[u32]| -> Vec<Seam> {
            xs.iter().map(|&x| Seam::Vertical(i64::from(x))).chain(ys.iter().map(|&y| Seam::Horizontal(i64::from(y)))).collect()
        };
        let halves = vec![Seam::Horizontal(i64::from(height / 2))];
        let tiled = SpatialInput::OverlappingCloudTiles;
        let whole_groups = grouped(&page, false, &whole, &local_rt, (SpatialInput::Whole, SpatialInput::Halves), halves.clone());
        let old_groups = grouped(&page, false, &old_mask, &local_rt, (SpatialInput::CloudTiles, SpatialInput::Halves),
            [seams(&old_xs, &old_ys), halves.clone()].concat());
        let new_groups = grouped(&page, false, &new_mask, &local_rt, (tiled, SpatialInput::Halves),
            [seams(&new_xs, &new_ys), halves.clone()].concat());
        // The full cloud path: overlapping SAM with the cloud's merged boxes.
        let cloud_groups = grouped(&page, false, &new_mask, &cloud_rt, (tiled, tiled), seams(&new_xs, &new_ys));
        // The same two as production groups them, for the crop comparison.
        let whole_run = grouped(&page, true, &whole, &local_rt, (SpatialInput::Whole, SpatialInput::Halves), halves.clone());
        let cloud_run = grouped(&page, true, &new_mask, &cloud_rt, (tiled, tiled), seams(&new_xs, &new_ys));
        let matched_groups = |other: &Grouping| {
            whole_groups
                .cleaning()
                .filter(|a| {
                    other.cleaning().any(|b| {
                        let inter = intersection(&a.bounds, &b.bounds);
                        inter * 2 >= area(&a.bounds) + area(&b.bounds) - inter
                    })
                })
                .count()
        };
        let (legacy_whole, legacy_whole_flagged) = legacy_regions(&page, &whole, &local_rt);
        let (legacy_tiled, legacy_tiled_flagged) = legacy_regions(&page, &old_mask, &local_rt);
        let entry = serde_json::json!({
            "page": path.file_name().unwrap().to_string_lossy(),
            "size": [width, height],
            "plans": {
                "old": { "tiles": old.len(), "pixels": old.iter().map(|t| u64::from(t.width) * u64::from(t.height)).sum::<u64>() },
                "overlap": { "tiles": plan.len(), "pixels": plan.iter().map(|p| u64::from(p.tile.width) * u64::from(p.tile.height)).sum::<u64>() },
            },
            "coverage": {
                "old": coverage(width, &whole, &old_mask, &old_xs, &old_ys),
                "overlap": coverage(width, &whole, &new_mask, &new_xs, &new_ys),
            },
            "boxes": {
                "local": local_rt.len(),
                "oldTilesRaw": old_rt.len(),
                "overlapRaw": tile_boxes.len(),
                "overlapMerged": cloud_rt.len(),
                "localMatchedByOld": matched(&local_rt, &old_rt),
                "localMatchedByMerged": matched(&local_rt, &cloud_rt),
                "mergedMatchedByLocal": matched(&cloud_rt, &local_rt),
            },
            "legacy": { "whole": legacy_whole, "wholeFlagged": legacy_whole_flagged,
                "tiled": legacy_tiled, "tiledFlagged": legacy_tiled_flagged },
            "grouped": {
                "whole": summary(&page, &whole_groups, &whole),
                "old": summary(&page, &old_groups, &old_mask),
                "overlap": summary(&page, &new_groups, &new_mask),
                "cloud": summary(&page, &cloud_groups, &new_mask),
                "wholeGroupsMatchedInOld": matched_groups(&old_groups),
                "wholeGroupsMatchedInOverlap": matched_groups(&new_groups),
                "wholeGroupsMatchedInCloud": matched_groups(&cloud_groups),
            },
            "crops": {
                "whole": crop_schedules(&whole_run, &whole),
                "cloud": crop_schedules(&cloud_run, &new_mask),
            },
            "models": {
                "whole": describe_models(&whole_groups.models),
                "old": describe_models(&old_groups.models),
                "overlap": describe_models(&new_groups.models),
                "cloud": describe_models(&cloud_groups.models),
            },
        });
        eprintln!("{}", serde_json::to_string(&entry).unwrap());
        report.push(entry);
    }
    let mut crops = serde_json::json!({});
    for entry in &report {
        summed(&mut crops, &entry["crops"]);
    }
    eprintln!("crop schedules, all pages: {crops}");
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}

/// Regroup the saved demo detections: their stored lettering is the only
/// mask evidence the snapshot kept (no raw SAM mask, no Ogkalu boxes), so
/// this is SAM-only layout grouping over the union of stored ink per page.
#[test]
#[ignore = "manual: a copy of the demo detect snapshot required"]
fn regroup_saved_snapshot_detections() {
    let dir = std::env::var("SNAPSHOT_JOB").expect("SNAPSHOT_JOB");
    let job = crate::project::Job::open(Path::new(&dir)).unwrap();
    let mut pages: std::collections::BTreeMap<usize, Vec<Mask>> = Default::default();
    for row in &job.project.detections {
        let loaded = job.load_detection(&row.id).unwrap().unwrap();
        pages.entry(row.source_idx).or_default().push(loaded.ink);
    }
    let (mut before, mut after, mut candidates) = (0usize, 0usize, 0usize);
    for (source, inks) in &pages {
        let (width, height) = (1136u32, 1601u32);
        let mut mask = vec![0u8; (width * height) as usize];
        for ink in inks {
            for y in ink.bounds.y.max(0)..ink.bounds.bottom().min(height as i64) {
                for x in ink.bounds.x.max(0)..ink.bounds.right().min(width as i64) {
                    if ink.contains(x, y) {
                        mask[y as usize * width as usize + x as usize] = 255;
                    }
                }
            }
        }
        let mut inputs = Inputs::new(width, height, Some(&mask));
        inputs.models = vec![ModelUse::new(EvidenceModel::SamTsL, Execution::Cloud, SpatialInput::CloudTiles)];
        let g = group(&inputs).unwrap();
        let clean = g.cleaning().count();
        eprintln!("source {source}: {} stored -> {} groups ({} held)", inks.len(), clean, g.candidates().count());
        before += inks.len();
        after += clean;
        candidates += g.candidates().count();
    }
    eprintln!("snapshot total: {before} stored detections -> {after} layout groups, {candidates} held candidates");
}

/// Offline comparison inputs on the exact copied pages used by the human
/// rectangle annotations, with a fresh whole-page SAM answer for each page.
#[test]
#[ignore = "manual: copied pages and local CTD, Full RT and SAM graphs required"]
fn annotated_model_evidence() {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::time::Instant;

    let pages = std::env::var("ANNOTATION_PAGES").expect("ANNOTATION_PAGES");
    let output = std::path::PathBuf::from(std::env::var("ANNOTATION_OUTPUT").expect("ANNOTATION_OUTPUT"));
    let sam_dir = std::path::PathBuf::from(std::env::var("ANNOTATION_SAM_DIR").expect("ANNOTATION_SAM_DIR"));
    std::fs::create_dir_all(&output).unwrap();
    crate::runtime::load(Path::new(&std::env::var("RT_RUNTIME").expect("RT_RUNTIME"))).unwrap();
    let cpu = crate::accel::Preference::CpuOnly;
    let mut ctd = crate::detect::Detector::open(Path::new(&std::env::var("CTD_GRAPH").expect("CTD_GRAPH")), cpu).unwrap();
    let mut rt = crate::rt_regions::FullRegions::open_cpu(Path::new(&std::env::var("RT_FULL_GRAPH").expect("RT_FULL_GRAPH"))).unwrap().0;
    let graph_hashes = ["koharu_samts_encoder.onnx", "koharu_samts_text_head.onnx"]
        .into_iter().map(|name| {
            let mut file = std::fs::File::open(sam_dir.join(name)).unwrap();
            let mut digest = Sha256::new();
            let mut buffer = [0u8; 1024 * 1024];
            loop {
                let size = file.read(&mut buffer).unwrap();
                if size == 0 { break; }
                digest.update(&buffer[..size]);
            }
            (name.to_string(), format!("{:x}", digest.finalize()))
        }).collect::<std::collections::BTreeMap<_, _>>();
    let mut sam_session = crate::sam_ts::SamTsSession::open_on(
        &sam_dir, crate::accel::Accelerator::Cpu, &crate::sam_ts::Cancellation::default(),
    ).unwrap();
    let mut report = Vec::new();
    for stem in ["01", "03", "04", "05", "21"] {
        let page_file = Path::new(&pages).join(format!("{stem}.png"));
        let bytes = std::fs::read(&page_file).unwrap();
        let page_hash = format!("{:x}", Sha256::digest(&bytes));
        let page = crate::image::decode(&bytes).unwrap();
        let now = Instant::now();
        let detection = ctd.detect(&page).unwrap();
        let ctd_ms = now.elapsed().as_secs_f64() * 1000.0;
        let pixels: Vec<u8> = detection.segmentation.levels.iter()
            .map(|&v| if v >= crate::detect::MASK_THRESHOLD { 255 } else { 0 }).collect();
        let now = Instant::now();
        let rt_boxes = rt.detect_halves(&page).unwrap();
        let rt_ms = now.elapsed().as_secs_f64() * 1000.0;
        let now = Instant::now();
        let sam = sam_session.infer(&page, &crate::sam_ts::Cancellation::default()).unwrap().mask;
        let sam_ms = now.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(sam.len(), pixels.len(), "SAM mask size differs from {stem} page pixels");
        let sam_hash = format!("{:x}", Sha256::digest(&sam));
        std::fs::write(output.join(format!("{stem}.ctd-mask.bin")), &pixels).unwrap();
        std::fs::write(output.join(format!("{stem}.sam-fresh.bin")), &sam).unwrap();
        std::fs::write(output.join(format!("{stem}.ctd-boxes.json")), serde_json::to_vec_pretty(
            &detection.boxes.iter().map(|b| serde_json::json!({"rect":b.rect,"confidence":b.confidence,"language":format!("{:?}",b.language)})).collect::<Vec<_>>()
        ).unwrap()).unwrap();
        std::fs::write(output.join(format!("{stem}.rt-full.json")), serde_json::to_vec_pretty(&rows(&rt_boxes)).unwrap()).unwrap();

        let mut variants = serde_json::Map::new();
        let mut add = |name: &str, mask: &[u8], pixel_model: EvidenceModel, with_ctd: bool, with_rt: bool| {
            let mut inputs = Inputs::new(page.width, page.height, Some(mask));
            inputs.pixel_model = pixel_model;
            inputs.page = Some(&page);
            inputs.ctd = if with_ctd { &detection.boxes } else { &[] };
            inputs.rt = if with_rt { &rt_boxes } else { &[] };
            inputs.models = [
                with_ctd.then(|| ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole)),
                with_rt.then(|| ModelUse::new(EvidenceModel::OgkaluFull, Execution::Local, SpatialInput::Halves)),
                (pixel_model == EvidenceModel::SamTsL).then(|| ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole)),
            ].into_iter().flatten().collect();
            let now = Instant::now();
            let grouping = group(&inputs).unwrap();
            let group_ms = now.elapsed().as_secs_f64() * 1000.0;
            let groups: Vec<_> = grouping.groups.iter().map(|g| serde_json::json!({
                "id":g.id,"bounds":g.bounds,"disposition":g.disposition,
                "letteringPixels":g.lettering_pixels,"estimated":g.estimated,
            })).collect();
            variants.insert(name.to_string(), serde_json::json!({
                "groupMs":group_ms,"cleanGroups":grouping.cleaning().count(),
                "candidates":grouping.candidates().count(),"groups":groups,
            }));
        };
        add("ctd", &pixels, EvidenceModel::Ctd, true, false);
        add("rtSam", &sam, EvidenceModel::SamTsL, false, true);
        add("allThree", &sam, EvidenceModel::SamTsL, true, true);
        report.push(serde_json::json!({
            "page":stem,"sourcePngSha256":page_hash,"size":[page.width,page.height],
            "ctdMs":ctd_ms,"rtFullMs":rt_ms,"samMs":sam_ms,"samMaskSha256":sam_hash,
            "samGraphSha256":graph_hashes,"ctdBoxes":detection.boxes.len(),
            "rtFullBoxes":rt_boxes.len(),"variants":variants,
        }));
    }
    std::fs::write(output.join("model-evidence.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}

/// The default selection (CTD with Ogkalu small, no SAM) through the grouping,
/// against the pre-grouping box path it replaced: CTD's segmentation is the
/// pixel evidence, every Ogkalu text box it misses gets a bounded estimate.
///
/// ```text
/// RT_RUNTIME=.../libonnxruntime.dylib CTD_GRAPH=.../comictextdetector.onnx \
/// RT_SMALL_GRAPH=.../comic-text-and-bubble-detector-detector-v4-s_int8.onnx \
/// GROUPING_PAGES=.../pages GROUPING_REPORT=.../report.json \
/// cargo test -p cleaner-core --release --lib text_groups::measure::default_models -- --ignored --nocapture
/// ```
#[test]
#[ignore = "manual: ONNX Runtime, CTD and small RT-DETR graphs and page directory required"]
fn default_models_through_the_grouping() {
    crate::runtime::load(Path::new(&std::env::var("RT_RUNTIME").expect("RT_RUNTIME"))).unwrap();
    let cpu = crate::accel::Preference::CpuOnly;
    let mut ctd_model =
        crate::detect::Detector::open(Path::new(&std::env::var("CTD_GRAPH").expect("CTD_GRAPH")), cpu).unwrap();
    let mut rt_model = crate::balloon::BalloonDetector::open(
        Path::new(&std::env::var("RT_SMALL_GRAPH").expect("RT_SMALL_GRAPH")),
        cpu,
    )
    .unwrap();
    let pages = std::env::var("GROUPING_PAGES").expect("GROUPING_PAGES");
    let mut names: Vec<_> = std::fs::read_dir(&pages).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    let mut report = Vec::new();
    for path in names.into_iter().filter(|p| p.extension().is_some_and(|e| e == "png")) {
        let page = crate::image::decode(&std::fs::read(&path).unwrap()).unwrap();
        let detection = ctd_model.detect(&page).unwrap();
        let rt = rt_model.detect(&page).unwrap();
        let pixels: Vec<u8> = detection
            .segmentation
            .levels
            .iter()
            .map(|&v| if v >= crate::detect::MASK_THRESHOLD { 255 } else { 0 })
            .collect();
        let mut inputs = Inputs::new(page.width, page.height, Some(&pixels));
        inputs.pixel_model = EvidenceModel::Ctd;
        inputs.page = Some(&page);
        inputs.rt = &rt;
        inputs.ctd = &detection.boxes;
        inputs.models = vec![
            ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole),
            ModelUse::new(EvidenceModel::OgkaluSmall, Execution::Local, SpatialInput::Whole),
        ];
        let grouping = group(&inputs).unwrap();
        let regions = crate::detect::build_regions_separated(detection.boxes.clone(), page.width, page.height, |a, b| {
            crate::balloon::merge_crosses_a_balloon(&page, &detection.segmentation, a, b)
        });
        let median = crate::detect::median_box_area(&detection.boxes);
        let adopted = legacy_adopted(&page, &regions, &rt, median);
        let lone: Vec<&TextGroup> = grouping.detector_only().collect();
        let estimated: Vec<&&TextGroup> = lone.iter().filter(|g| g.estimated).collect();
        let estimate_share = estimated
            .iter()
            .map(|g| {
                let anchor = g.anchor.unwrap_or(g.bounds);
                f64::from(g.lettering_pixels) / area(&anchor).max(1) as f64
            })
            .fold(0.0, f64::max);
        let candidates: Vec<&TextGroup> = grouping.candidates().collect();
        let mut candidate_reasons = std::collections::BTreeMap::<&str, usize>::new();
        for candidate in &candidates {
            let key = candidate.reasons.iter().min().map_or("none", |reason| reason.key());
            *candidate_reasons.entry(key).or_default() += 1;
        }
        // Ogkalu small alone: every text box is estimated.
        let mut alone = Inputs::new(page.width, page.height, None);
        alone.page = Some(&page);
        alone.rt = &rt;
        alone.models = vec![ModelUse::new(EvidenceModel::OgkaluSmall, Execution::Local, SpatialInput::Whole)];
        let alone = group(&alone).unwrap();
        let alone_groups: Vec<&TextGroup> = alone.detector_only().collect();
        let alone_estimated: Vec<&&TextGroup> = alone_groups.iter().filter(|g| g.estimated).collect();
        let share = |g: &TextGroup| f64::from(g.lettering_pixels) / area(&g.anchor.unwrap_or(g.bounds)).max(1) as f64;
        let (listed_twice, lost) = listed_twice_and_lost(&grouping);
        let row = serde_json::json!({
            "models": describe_models(&grouping.models),
            "crops": crop_schedules(&grouping, &pixels),
            "rtOnly": {
                "spanningTwoBubblesByLettering": spanning_lettering(&alone),
                "flaggedCrossing": alone.groups.iter().filter(|g| g.reasons.contains(&ReviewReason::CrossesBalloon)).count(),
                "groups": alone_groups.len(),
                "candidates": alone.candidates().count(),
                "splitParts": alone.groups.iter().filter(|g| g.split.is_some()).count(),
                "estimated": alone_estimated.len(),
                "estimatePixels": alone_estimated.iter().map(|g| u64::from(g.lettering_pixels)).sum::<u64>(),
                "maxEstimateShareOfBox": alone_estimated.iter().map(|g| share(g)).fold(0.0, f64::max),
                "estimatePixelsUnderCtdSegmentation": alone_estimated.iter().map(|g| {
                    let m = alone.lettering(g);
                    (m.bounds.y..m.bounds.bottom()).flat_map(|y| (m.bounds.x..m.bounds.right()).map(move |x| (x, y)))
                        .filter(|&(x, y)| m.contains(x, y) && pixels[y as usize * page.width as usize + x as usize] != 0)
                        .count() as u64
                }).sum::<u64>(),
            },
            "page": path.file_name().unwrap().to_string_lossy(),
            "ctdBoxes": detection.boxes.len(),
            "rtTextBoxes": rt.iter().filter(|b| b.class != BalloonClass::Bubble).count(),
            "legacy": { "regions": regions.len(), "adopted": adopted.len() },
            "grouped": {
                "cleaningGroups": grouping.cleaning().count(),
                "detectorOnly": lone.len(),
                "estimated": estimated.len(),
                "estimatePixels": estimated.iter().map(|g| u64::from(g.lettering_pixels)).sum::<u64>(),
                "maxEstimateShareOfBox": estimate_share,
                "candidates": candidates.len(),
                "candidateReasons": candidate_reasons,
                "candidatePixels": candidates.iter().map(|g| u64::from(g.lettering_pixels)).sum::<u64>(),
                "estimatedWithMaskPixels": grouping.cleaning().filter(|g| g.estimated).count(),
                "spanningTwoBubbleBoxes": spanning_bubbles(&grouping),
                "spanningTwoBubblesByLettering": spanning_lettering(&grouping),
                "componentsListedTwice": listed_twice,
                "componentsLost": lost,
                "flaggedCrossing": grouping.cleaning().filter(|g| g.reasons.contains(&ReviewReason::CrossesBalloon)).count(),
                "letteringPixelsInCleaningGroups": grouping.cleaning().map(|g| u64::from(g.lettering_pixels)).sum::<u64>(),
                "segPixels": pixels.iter().filter(|v| **v != 0).count(),
            },
        });
        println!("{row}");
        report.push(row);
    }
    let mut crops = serde_json::json!({});
    for row in &report {
        summed(&mut crops, &row["crops"]);
    }
    println!("crop schedules, all pages: {crops}");
    std::fs::write(std::env::var("GROUPING_REPORT").expect("GROUPING_REPORT"), serde_json::to_vec_pretty(&report).unwrap())
        .unwrap();
}

/* ------------------------------------------------------------------ */
/* The comparison's own tests: no model, no page directory            */
/* ------------------------------------------------------------------ */

fn block(mask: &mut [u8], width: u32, rect: Rect) {
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            mask[y as usize * width as usize + x as usize] = 255;
        }
    }
}

/// Scattered glyph-sized blobs from a fixed-seed generator, a bar wider than
/// two cores and a column taller than one.
fn scattered(width: u32, height: u32) -> Vec<u8> {
    let mut mask = vec![0u8; width as usize * height as usize];
    let mut seed = 0x2545_f491_u64;
    let mut next = |bound: u32| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 33) % u64::from(bound)) as i64
    };
    for _ in 0..160 {
        let (x, y) = (next(width - 40), next(height - 40));
        let (w, h) = (4 + next(30) as u32, 4 + next(30) as u32);
        block(&mut mask, width, Rect::new(x, y, w, h));
    }
    block(&mut mask, width, Rect::new(20, 1500, 1100, 12));
    block(&mut mask, width, Rect::new(1180, 300, 10, 700));
    mask
}

/// Which jobs write each lettering pixel: a member of the job, inside its core.
fn writers_per_pixel(width: u32, labeled: &Labeled, jobs: &[CropJob]) -> Vec<usize> {
    labeled
        .labels
        .iter()
        .enumerate()
        .filter(|(_, &l)| l != 0)
        .map(|(i, &l)| {
            let (x, y) = ((i % width as usize) as i64, (i / width as usize) as i64);
            jobs.iter().filter(|job| job.members.contains(&l) && job.core.contains(x, y)).count()
        })
        .collect()
}

#[test]
fn koharu_packing_covers_every_pixel_once_within_the_bound() {
    let (width, height) = (1300, 1600);
    let mask = scattered(width, height);
    let labeled = label(width, height, &mask);
    let jobs = koharu_pack(width, height, &labeled);
    assert!(writers_per_pixel(width, &labeled, &jobs).iter().all(|&n| n == 1), "a pixel written twice or never");
    let page = Rect::new(0, 0, width, height);
    for job in &jobs {
        assert!(job.core.w <= KOHARU_CORE && job.core.h <= KOHARU_CORE, "{job:?}");
        assert!(job.crop.w <= KOHARU_CORE + 256 && job.crop.h <= KOHARU_CORE + 256, "{job:?}");
        assert_eq!(clipped(job.crop, width, height), job.crop, "a crop left the page");
        assert!(intersection(&job.crop, &page) > 0 && intersection(&job.crop, &job.core) == area(&job.core));
    }
    // Each bounded component sits in exactly one job; only the two oversized
    // ones are split, the 1100 px bar into three cells and the 700 px column
    // into two.
    let oversized: Vec<u32> = labeled
        .components
        .iter()
        .enumerate()
        .filter(|(_, (r, _))| r.w > KOHARU_CORE || r.h > KOHARU_CORE)
        .map(|(k, _)| k as u32 + 1)
        .collect();
    assert_eq!(oversized.len(), 2);
    for label in 1..=labeled.components.len() as u32 {
        let holding = jobs.iter().filter(|job| job.members.contains(&label)).count();
        let expected = match oversized.iter().position(|&l| l == label) {
            Some(_) if labeled.components[label as usize - 1].0.w > KOHARU_CORE => 3,
            Some(_) => 2,
            None => 1,
        };
        assert_eq!(holding, expected, "component {label}");
    }
    assert!(jobs.len() < labeled.components.len(), "nothing was packed together");
    // Deterministic: the same mask packs the same way again.
    assert_eq!(koharu_pack(width, height, &labeled), jobs);
    assert_eq!(koharu_pack(width, height, &label(width, height, &scattered(width, height))), jobs);
}

#[test]
fn a_split_cell_crops_around_the_pixels_it_holds() {
    let (width, height) = (1400, 400);
    let mut mask = vec![0u8; (width * height) as usize];
    block(&mut mask, width, Rect::new(50, 100, 800, 20));
    let labeled = label(width, height, &mask);
    let jobs = koharu_pack(width, height, &labeled);
    assert_eq!(jobs.iter().map(|job| job.core).collect::<Vec<_>>(),
        vec![Rect::new(50, 100, 512, 20), Rect::new(562, 100, 288, 20)]);
    assert_eq!(jobs[1].crop, Rect::new(562 - 128, 0, 288 + 256, 120 + 128));
    // The same bar as one free-text group: one job of ours, two of Koharu's,
    // each cell's context showing the other cell's lettering.
    let rt = [BalloonBox { rect: Rect::new(45, 95, 810, 30), class: BalloonClass::TextFree, score: 0.9 }];
    let mut inputs = Inputs::new(width, height, Some(&mask));
    inputs.rt = &rt;
    inputs.models = vec![
        ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole),
        ModelUse::new(EvidenceModel::OgkaluFull, Execution::Local, SpatialInput::Halves),
    ];
    let comparison = crop_schedules(&group(&inputs).unwrap(), &mask);
    assert_eq!((comparison["ours"]["jobs"].as_u64(), comparison["koharu"]["jobs"].as_u64()), (Some(1), Some(2)));
    assert_eq!(comparison["koharu"]["componentsSplitAcrossJobs"], 1);
    assert_eq!(comparison["koharu"]["componentsWrittenTwice"], 0);
    assert_eq!(comparison["koharu"]["cropSeesAnotherJobsLettering"], 2);
    assert_eq!(comparison["ours"]["cropSeesAnotherJobsLettering"], 0);
}

/// Three balloons side by side, a column of glyphs in each, the first two
/// within one Koharu core and the third far off. We send three crops, one per
/// balloon; Koharu packs the first two balloons' glyphs into one job whose
/// crop and writes span both balloons and both groups.
#[test]
fn packing_merges_what_grouping_keeps_apart() {
    let (width, height) = (1400, 400);
    let mut mask = vec![0u8; (width * height) as usize];
    let mut rt = Vec::new();
    for left in [20, 230, 1100] {
        for y in [80, 110, 140, 170, 200, 230] {
            block(&mut mask, width, Rect::new(left + 80, y, 20, 20));
        }
        rt.push(BalloonBox { rect: Rect::new(left, 20, 200, 300), class: BalloonClass::Bubble, score: 0.9 });
        rt.push(BalloonBox { rect: Rect::new(left + 60, 60, 60, 220), class: BalloonClass::TextInBubble, score: 0.9 });
    }
    let mut inputs = Inputs::new(width, height, Some(&mask));
    inputs.rt = &rt;
    inputs.models = vec![
        ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole),
        ModelUse::new(EvidenceModel::OgkaluFull, Execution::Local, SpatialInput::Halves),
    ];
    let grouping = group(&inputs).unwrap();
    assert_eq!(grouping.cleaning().count(), 3);
    let c = crop_schedules(&grouping, &mask);
    assert_eq!((c["components"].as_u64(), c["letteringPixels"].as_u64()), (Some(18), Some(18 * 400)));
    let (ours, koharu) = (&c["ours"], &c["koharu"]);
    assert_eq!((ours["jobs"].as_u64(), koharu["jobs"].as_u64()), (Some(3), Some(2)));
    assert_eq!((ours["mixesGroups"].as_u64(), koharu["mixesGroups"].as_u64()), (Some(0), Some(1)));
    assert_eq!((ours["writesTwoBalloons"].as_u64(), koharu["writesTwoBalloons"].as_u64()), (Some(0), Some(1)));
    assert_eq!((ours["cropCoversTwoBalloons"].as_u64(), koharu["cropCoversTwoBalloons"].as_u64()), (Some(0), Some(1)));
    for schedule in [ours, koharu] {
        assert_eq!(schedule["componentsUncovered"], 0);
        assert_eq!(schedule["componentsWrittenTwice"], 0);
        assert_eq!(schedule["cropSeesAnotherJobsLettering"], 0);
    }
    // Ours is exactly the cloud path's crop of each group's lettering: a
    // 20 x 170 column grown by 5 and given 128 a side, snapped to 16.
    let letters = lettering_of_cleaning(&grouping);
    let jobs = our_jobs(&letters);
    let render = crate::engines::render::Preprocessing::CURRENT;
    for (job, group) in jobs.iter().zip(grouping.cleaning()) {
        assert_eq!(job.core, group.bounds);
        assert_eq!(Some(job.crop), render.crop_of_hole(group.bounds, width, height));
        assert_eq!((job.crop.w, job.crop.h), (288, 448));
    }
    assert_eq!(ours["cropPixels"], 3 * 288 * 448);
    assert_eq!(ours["maxCropSide"], 448);
    // Koharu's crops stop at the page: 0..458 by 0..378, and 1052..1328.
    assert_eq!(koharu["cropPixels"], 458 * 378 + 276 * 378);
    assert_eq!(koharu["corePixels"], 230 * 170 + 20 * 170);
    assert_eq!(c["koharuWholeMask"]["jobs"], 2);
    let mut totals = serde_json::json!({});
    summed(&mut totals, &c);
    summed(&mut totals, &c);
    assert_eq!(totals["ours"]["jobs"], 6);
    assert_eq!(totals["ours"]["maxCropSide"], 448);
}

/// Each report names the exact checkpoint and spatial input behind a grouping.
#[test]
fn harness_reports_name_their_models() {
    let page = Raster {
        width: 64,
        height: 64,
        mode: crate::image::ColorMode::Gray,
        depth: crate::image::BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data: vec![255; 64 * 64],
    };
    let mask = vec![0u8; 64 * 64];
    let g = grouped(&page, false, &mask, &[], (SpatialInput::OverlappingCloudTiles, SpatialInput::OverlappingCloudTiles), Vec::new());
    let named = serde_json::to_value(describe_models(&g.models)).unwrap();
    assert_eq!(named[0]["checkpoint"], "mayocream/koharu-text-sam-ts-l");
    assert_eq!(named[0]["revision"], SAM_TS_L_REVISION);
    assert_eq!(named[0]["execution"], "local");
    assert!(named[0]["described"].as_str().unwrap().contains("[local, cloud tiles 1.1.0: 1024 tiles overlapping"));
    assert_eq!(named[1]["checkpoint"], "ogkalu/comic-text-and-bubble-detector detector.onnx (RT-DETR v2, FP32)");
    assert_eq!(named[1]["revision"], OGKALU_FULL_REVISION);
}
