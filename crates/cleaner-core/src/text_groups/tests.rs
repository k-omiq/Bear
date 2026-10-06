//! The synthetic many-glyph fixture and the contract's invariants.
use super::*;

const W: u32 = 1300;
const H: u32 = 1100;

fn fill(mask: &mut [u8], rect: Rect) {
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            mask[y as usize * W as usize + x as usize] = 255;
        }
    }
}

/// A glyph with a hole: one component that is not a rectangle.
fn ring(mask: &mut [u8], x: i64, y: i64, s: u32) {
    fill(mask, Rect::new(x, y, s, s));
    let inner = s as i64 / 3;
    for yy in y + inner..y + s as i64 - inner {
        for xx in x + inner..x + s as i64 - inner {
            mask[yy as usize * W as usize + xx as usize] = 0;
        }
    }
}

/// A glyph drawn as two disconnected strokes: two components.
fn two_strokes(mask: &mut [u8], x: i64, y: i64, s: u32) {
    fill(mask, Rect::new(x, y, s / 3, s));
    fill(mask, Rect::new(x + 2 * s as i64 / 3, y, s / 3, s));
}

fn disk(mask: &mut [u8], cx: i64, cy: i64, r: i64) {
    for y in cy - r..=cy + r {
        for x in cx - r..=cx + r {
            if (x - cx).pow(2) + (y - cy).pow(2) <= r * r {
                mask[y as usize * W as usize + x as usize] = 255;
            }
        }
    }
}

fn rt(x: i64, y: i64, w: u32, h: u32, class: BalloonClass, score: f32) -> BalloonBox {
    BalloonBox { rect: Rect::new(x, y, w, h), class, score }
}

fn ctd(x: i64, y: i64, w: u32, h: u32) -> DetBox {
    DetBox { rect: Rect::new(x, y, w, h), confidence: 0.8, language: DetectedLanguage::Japanese }
}

struct Fixture {
    mask: Vec<u8>,
    rt: Vec<BalloonBox>,
    ctd: Vec<DetBox>,
    /// Named probe points, one pixel inside each labelled piece of lettering.
    art: (i64, i64),
    speck: (i64, i64),
    stray_b: (i64, i64),
    bang: (i64, i64),
    period: (i64, i64),
}

/// Several balloons (one vertical, one horizontal, touching), punctuation and
/// disconnected strokes, a free caption, a balloon and a caption each cut by a
/// cloud tile seam (x = 1024, y = 1024), text a box detector missed, a CTD-only
/// line, a text box with no lettering under it, a speck, and a piece of
/// isolated artwork.
fn fixture() -> Fixture {
    let mut mask = vec![0u8; (W * H) as usize];
    // Balloon A: two vertical columns (right to left), one glyph in two
    // strokes, an exclamation mark left of the box and a period below it.
    for y in [80, 110, 140, 170, 200] {
        ring(&mut mask, 280, y, 24);
    }
    for y in [80, 110, 140] {
        ring(&mut mask, 245, y, 24);
    }
    two_strokes(&mut mask, 245, 170, 24);
    fill(&mut mask, Rect::new(228, 80, 6, 18));
    fill(&mut mask, Rect::new(228, 101, 6, 5));
    fill(&mut mask, Rect::new(296, 232, 5, 5));
    // Balloon B, 2 px right of A: a horizontal row, and a glyph near A's
    // column that belongs to B.
    for x in [350, 380, 410, 440] {
        ring(&mut mask, x, 100, 24);
    }
    ring(&mut mask, 318, 100, 24);
    // A free caption.
    for k in 0..8 {
        ring(&mut mask, 610 + 34 * k, 505, 24);
    }
    // Balloon C, cut by the x = 1024 seam: one glyph straddles it.
    for y in [340, 370, 400] {
        ring(&mut mask, 1012, y, 24);
        ring(&mut mask, 1050, y, 24);
    }
    // A caption cut by the y = 1024 seam.
    for x in [110, 140, 170, 200] {
        ring(&mut mask, x, 1012, 24);
    }
    // Text no box detector saw.
    for x in [100, 130, 160] {
        ring(&mut mask, x, 900, 24);
    }
    // A CTD-only line.
    for x in [810, 840, 870] {
        ring(&mut mask, x, 155, 20);
    }
    // Isolated artwork and a speck.
    disk(&mut mask, 700, 800, 30);
    fill(&mut mask, Rect::new(1200, 50, 2, 2));

    let rt = vec![
        rt(40, 40, 280, 340, BalloonClass::Bubble, 0.9),
        rt(240, 75, 70, 152, BalloonClass::TextInBubble, 0.88),
        rt(322, 40, 220, 160, BalloonClass::Bubble, 0.91),
        rt(345, 95, 125, 34, BalloonClass::TextInBubble, 0.86),
        rt(600, 500, 300, 40, BalloonClass::TextFree, 0.8),
        rt(960, 300, 64, 200, BalloonClass::Bubble, 0.8),
        rt(1024, 300, 90, 200, BalloonClass::Bubble, 0.82),
        rt(975, 330, 49, 100, BalloonClass::TextInBubble, 0.7),
        rt(1024, 330, 70, 100, BalloonClass::TextInBubble, 0.75),
        rt(100, 990, 140, 34, BalloonClass::TextFree, 0.7),
        rt(100, 1024, 140, 30, BalloonClass::TextFree, 0.72),
        rt(1150, 700, 60, 30, BalloonClass::TextFree, 0.6),
    ];
    let ctd = vec![ctd(242, 78, 66, 146), ctd(805, 150, 90, 30)];
    Fixture {
        mask,
        rt,
        ctd,
        art: (700, 800),
        speck: (1200, 50),
        stray_b: (319, 101),
        bang: (229, 81),
        period: (297, 233),
    }
}

fn models() -> Vec<ModelUse> {
    vec![
        ModelUse::new(EvidenceModel::SamTsL, Execution::Cloud, SpatialInput::OverlappingCloudTiles),
        ModelUse::new(EvidenceModel::OgkaluFull, Execution::Cloud, SpatialInput::OverlappingCloudTiles),
        ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole),
    ]
}

fn run(f: &Fixture, rt: &[BalloonBox], ctd_boxes: &[DetBox]) -> Grouping {
    let mut inputs = Inputs::new(W, H, Some(&f.mask));
    inputs.rt = rt;
    inputs.ctd = ctd_boxes;
    inputs.models = models();
    inputs.seams = vec![Seam::Vertical(1024), Seam::Horizontal(1024)];
    group(&inputs).unwrap()
}

fn group_at(grouping: &Grouping, (x, y): (i64, i64)) -> Option<&TextGroup> {
    let label = grouping.labels[y as usize * W as usize + x as usize];
    assert_ne!(label, 0, "probe ({x}, {y}) is not lettering");
    let id = grouping.components[label as usize - 1].group.as_ref()?;
    grouping.groups.iter().find(|g| &g.id == id)
}

fn with_box<'a>(grouping: &'a Grouping, id: &str) -> &'a TextGroup {
    grouping.groups.iter().find(|g| g.box_ids.iter().any(|b| b == id)).unwrap_or_else(|| panic!("no group has {id}"))
}

#[test]
fn many_glyphs_land_in_their_intended_groups() {
    let f = fixture();
    let g = run(&f, &f.rt, &f.ctd);

    // Balloon A: 13 glyph components (8 rings, 2 strokes, the "!" in two, the
    // period) are one group, with the duplicate CTD box folded in.
    let a = with_box(&g, "rt-0001");
    assert_eq!(a.origin, GroupOrigin::TextBox);
    assert_eq!(a.disposition, Disposition::Clean);
    assert_eq!(a.component_ids.len(), 8 + 2 + 2 + 1, "{:?}", a.component_ids);
    assert_eq!(a.box_ids, vec!["ctd-0000".to_owned(), "rt-0001".to_owned()]);
    assert_eq!(a.bubble.as_deref(), Some("rt-0000"));
    assert_eq!(group_at(&g, f.bang).map(|x| &x.id), Some(&a.id), "punctuation outside the box attaches");
    assert_eq!(group_at(&g, f.period).map(|x| &x.id), Some(&a.id));
    assert!(a.reasons.is_empty(), "{:?}", a.reasons);

    // Balloon B keeps its own stray glyph: the balloon wall wins over distance.
    let b = with_box(&g, "rt-0003");
    assert_eq!(b.component_ids.len(), 5);
    assert_eq!(group_at(&g, f.stray_b).map(|x| &x.id), Some(&b.id));
    assert_ne!(a.id, b.id);

    // The caption is one group, outside every balloon.
    let caption = with_box(&g, "rt-0004");
    assert_eq!((caption.component_ids.len(), caption.bubble.as_deref()), (8, None));

    // Seam-cut boxes are one group each: no duplicate job per tile.
    let c = with_box(&g, "rt-0007");
    assert_eq!(c.box_ids, vec!["rt-0007".to_owned(), "rt-0008".to_owned()]);
    assert_eq!(c.component_ids.len(), 6);
    let low = with_box(&g, "rt-0009");
    assert_eq!(low.box_ids, vec!["rt-0009".to_owned(), "rt-0010".to_owned()]);
    assert_eq!(low.component_ids.len(), 4);

    // CTD anchors what Ogkalu missed.
    let line = with_box(&g, "ctd-0001");
    assert_eq!((line.origin, line.component_ids.len()), (GroupOrigin::CtdBox, 3));

    // Text no box claimed is cleaned from the mask, grouped by layout, with
    // its reason for review.
    let missed = group_at(&g, (101, 901)).unwrap();
    assert_eq!(
        (missed.origin, missed.disposition, missed.component_ids.len()),
        (GroupOrigin::MaskLayout, Disposition::Clean, 3)
    );
    assert_eq!(missed.reasons, vec![ReviewReason::UnassignedComponent]);

    // The mask decides: an island the mask calls lettering is cleaned too,
    // with the same reason. A speck is evidence only.
    let art = group_at(&g, f.art).unwrap();
    assert_eq!((art.disposition, art.reasons.clone()), (Disposition::Clean, vec![ReviewReason::UnassignedComponent]));
    assert!(group_at(&g, f.speck).is_none());

    // A text box with no lettering under it says so.
    let empty = with_box(&g, "rt-0011");
    assert_eq!((empty.lettering_pixels, empty.reasons.clone()), (0, vec![ReviewReason::MaskMissingUnderTextBox]));

    // No component is in two groups, and no group spans two balloons.
    let mut seen = BTreeSet::new();
    for group in &g.groups {
        for id in &group.component_ids {
            assert!(seen.insert(id.clone()), "{id} in two groups");
        }
    }
    assert_eq!(g.cleaning().count(), 8);
}

#[test]
fn lettering_is_the_exact_glyph_union_and_never_touches_protected_pixels() {
    let f = fixture();
    let g = run(&f, &f.rt, &f.ctd);
    let art = group_at(&g, f.art).unwrap().clone();
    for group in g.cleaning() {
        let lettering = g.lettering(group);
        assert_eq!(lettering.count() as u32, group.lettering_pixels);
        assert!((lettering.count() as u64) < area(&lettering.bounds) as u64, "a solid rectangle");
        for y in lettering.bounds.y..lettering.bounds.bottom() {
            for x in lettering.bounds.x..lettering.bounds.right() {
                let on = f.mask[y as usize * W as usize + x as usize] != 0;
                assert!(!lettering.contains(x, y) || on, "lettering outside the source mask");
            }
        }
        if group.id != art.id {
            assert!(!lettering.contains(f.art.0, f.art.1), "{} took the artwork", group.id);
        }
        assert!(!lettering.contains(f.speck.0, f.speck.1));
    }
    // The island's own pixels are still in the evidence.
    assert_eq!(g.lettering(&art).count() as u32, art.lettering_pixels);
}

#[test]
fn grouping_does_not_depend_on_input_order() {
    let f = fixture();
    let first = run(&f, &f.rt, &f.ctd);
    let mut rt = f.rt.clone();
    rt.reverse();
    let mut ctd_boxes = f.ctd.clone();
    ctd_boxes.reverse();
    let second = run(&f, &rt, &ctd_boxes);
    let shape = |g: &Grouping| {
        g.groups
            .iter()
            .map(|t| (t.id.clone(), t.component_ids.clone(), t.disposition, t.reasons.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&first), shape(&second));
}

#[test]
fn a_group_keeps_its_identity_in_an_offset_crop() {
    let f = fixture();
    let whole = run(&f, &f.rt, &[]);
    // The same evidence cut to a crop starting at (500, 400), placed there.
    let (dx, dy) = (500i64, 400i64);
    let (cw, ch) = (W - dx as u32, H - dy as u32);
    let mut crop = vec![0u8; (cw * ch) as usize];
    for y in 0..ch as i64 {
        for x in 0..cw as i64 {
            crop[(y * cw as i64 + x) as usize] = f.mask[((y + dy) * W as i64 + x + dx) as usize];
        }
    }
    let boxes: Vec<BalloonBox> = f
        .rt
        .iter()
        .filter(|b| b.rect.x >= dx && b.rect.y >= dy)
        .map(|b| BalloonBox { rect: Rect::new(b.rect.x - dx, b.rect.y - dy, b.rect.w, b.rect.h), ..b.clone() })
        .collect();
    let mut inputs = Inputs::new(cw, ch, Some(&crop));
    inputs.origin = (dx, dy);
    inputs.rt = &boxes;
    inputs.models = models();
    let cropped = group(&inputs).unwrap();
    let caption = with_box(&whole, "rt-0004");
    assert!(cropped.groups.iter().any(|g| g.id == caption.id), "the caption changed identity");
}

#[test]
fn evidence_round_trips_and_rebuilds_each_groups_lettering() {
    let f = fixture();
    let g = run(&f, &f.rt, &f.ctd);
    let blob = g.evidence(Some(&f.mask)).unwrap();
    assert_eq!(blob.reference(), format!("evidence/{}.mtev", blob.sha256));
    let decoded = EvidenceFile::decode(&blob.bytes).unwrap();
    // A version 1 file (no PNG length, no owner plane) still decodes.
    let length = u32::from_le_bytes(blob.bytes[5..9].try_into().unwrap()) as usize;
    let mut old = b"MTEV1".to_vec();
    old.extend_from_slice(&blob.bytes[5..9 + length]);
    old.extend_from_slice(&blob.bytes[9 + length + 4..]);
    let older = EvidenceFile::decode(&old).unwrap();
    assert_eq!((older.mask.len(), older.grouping.groups.len()), (decoded.mask.len(), decoded.grouping.groups.len()));
    assert_eq!(decoded.mask.iter().filter(|v| **v != 0).count(), f.mask.iter().filter(|v| **v != 0).count());
    assert_eq!(decoded.grouping.boxes.len(), f.rt.len() + f.ctd.len(), "raw boxes, bubbles included");
    for group in &g.groups {
        let record = g.record(group, Some(&blob));
        assert_eq!(record.evidence_sha256.as_deref(), Some(blob.sha256.as_str()));
        let rebuilt = decoded.lettering(&record).unwrap();
        let digest = format!("{:x}", Sha256::digest(crate::project::buffers::encode_mask(&rebuilt)));
        assert_eq!(digest, record.lettering_sha256, "{}", group.id);
        // source mask -> components -> group -> record stays traceable.
        for id in &record.components {
            let component = decoded.grouping.components.iter().find(|c| &c.id == id).unwrap();
            assert_eq!(component.group.as_deref(), Some(group.id.as_str()));
        }
        for b in &record.boxes {
            assert!(decoded.grouping.boxes.iter().any(|raw| raw.id == b.id && raw.rect == b.rect));
        }
    }
}

#[test]
fn sam_only_groups_by_layout_and_flags_lone_islands() {
    let mut mask = vec![0u8; (W * H) as usize];
    // A vertical column, a second column beside it, and one lone glyph.
    for y in [100, 128, 156, 184] {
        ring(&mut mask, 300, y, 24);
        ring(&mut mask, 268, y, 24);
    }
    fill(&mut mask, Rect::new(305, 212, 5, 5));
    ring(&mut mask, 800, 600, 24);
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.models = vec![ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole)];
    let g = group(&inputs).unwrap();
    let clean: Vec<&TextGroup> = g.cleaning().collect();
    assert_eq!(clean.len(), 2);
    assert_eq!(clean[0].component_ids.len(), 9, "two columns and their period are one block");
    assert!(clean[0].reasons.is_empty());
    assert_eq!(g.candidates().count(), 0);
    assert_eq!(clean[1].reasons, vec![ReviewReason::IsolatedComponent], "the lone glyph is cleaned, with its reason");
    assert_eq!(g.record(clean[1], None).review_key(), None, "provenance, not a review flag");
}

#[test]
fn an_oversized_group_splits_into_disjoint_parts_under_the_limit() {
    let mut mask = vec![0u8; (W * H) as usize];
    for k in 0..30 {
        ring(&mut mask, 400, 20 + 34 * k, 24);
    }
    let boxes = [rt(390, 10, 50, 1040, BalloonClass::TextFree, 0.9)];
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.rt = &boxes;
    inputs.max_side = 400;
    let g = group(&inputs).unwrap();
    let parts: Vec<&TextGroup> = g.cleaning().collect();
    assert!(parts.len() >= 3);
    let mut all = BTreeSet::new();
    for part in &parts {
        assert!(part.bounds.h <= 400 && part.split.as_ref().is_some_and(|s| s.of as usize == parts.len()));
        for id in &part.component_ids {
            assert!(all.insert(id.clone()));
        }
    }
    assert_eq!(all.len(), 30);
}

#[test]
fn a_component_leaving_its_balloon_is_flagged_not_silently_taken() {
    let mut mask = vec![0u8; (W * H) as usize];
    ring(&mut mask, 120, 120, 24);
    // Lettering fused with a stroke that runs out of the balloon.
    fill(&mut mask, Rect::new(150, 125, 100, 8));
    fill(&mut mask, Rect::new(150, 120, 24, 24));
    let boxes = [
        rt(100, 100, 100, 80, BalloonClass::Bubble, 0.9),
        rt(115, 115, 90, 34, BalloonClass::TextInBubble, 0.9),
    ];
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.rt = &boxes;
    let g = group(&inputs).unwrap();
    let text = with_box(&g, "rt-0001");
    assert_eq!(text.reasons, vec![ReviewReason::CrossesBalloon]);
    assert_eq!(GroupRecord { reasons: text.reasons.clone(), ..g.record(text, None) }.review_key(),
        Some("review.reason.crossesBalloon"));
}

#[test]
fn the_block_median_comes_from_blocks_not_glyphs() {
    let f = fixture();
    let g = run(&f, &f.rt, &f.ctd);
    let median = g.block_median_area();
    // A glyph is 576 px; every block here is several glyphs.
    assert!(median > 2_000, "{median}");
    for group in g.cleaning() {
        assert!(!g.region(group, median).flagged_large, "{} flagged", group.id);
    }
}

/* ---- detector-only lettering: CTD pixels and bounded ink estimates ---- */

/// White paper with dark horizontal strokes.
fn paper(width: u32, height: u32, strokes: &[Rect]) -> crate::image::Raster {
    let mut data = vec![255u8; (width * height) as usize];
    for stroke in strokes {
        for y in stroke.y..stroke.bottom() {
            for x in stroke.x..stroke.right() {
                data[y as usize * width as usize + x as usize] = 20;
            }
        }
    }
    crate::image::Raster {
        width,
        height,
        mode: crate::image::ColorMode::Gray,
        depth: crate::image::BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data,
    }
}

fn ogkalu_only() -> Vec<ModelUse> {
    vec![ModelUse::new(EvidenceModel::OgkaluSmall, Execution::Local, SpatialInput::Whole)]
}

#[test]
fn text_boxes_without_pixels_get_exclusive_ink_estimates_never_rectangles() {
    // Two free captions whose boxes touch (their tight tiers overlap by eight
    // rows, with a stroke running through the overlap) and a box over flat
    // paper.
    let strokes = [
        Rect::new(110, 105, 40, 3),
        Rect::new(110, 115, 40, 3),
        Rect::new(110, 125, 40, 6),
        Rect::new(110, 140, 40, 3),
        Rect::new(110, 150, 40, 3),
    ];
    let page = paper(300, 300, &strokes);
    let rt_boxes = [
        rt(100, 100, 60, 30, BalloonClass::TextFree, 0.9),
        rt(100, 128, 60, 30, BalloonClass::TextFree, 0.9),
        rt(200, 200, 60, 30, BalloonClass::TextFree, 0.9),
    ];
    let mut inputs = Inputs::new(300, 300, None);
    inputs.page = Some(&page);
    inputs.rt = &rt_boxes;
    inputs.models = ogkalu_only();
    let g = group(&inputs).unwrap();
    assert_eq!(g.cleaning().count(), 0, "no pixel model ran");
    let groups: Vec<&TextGroup> = g.detector_only().collect();
    assert_eq!(groups.len(), 3, "the touching captions stay two groups");
    let mut owner = vec![0usize; 300 * 300];
    for (k, group) in groups.iter().enumerate() {
        let lettering = g.lettering(group);
        assert_eq!(lettering.count() as u32, group.lettering_pixels);
        let anchor = group.anchor.unwrap_or(group.bounds);
        if anchor.x == 200 {
            assert!(!group.estimated && lettering.is_empty(), "flat paper has no ink");
            continue;
        }
        assert!(group.estimated && group.lettering_pixels > 0);
        let tight = crate::detect::tight_rect(anchor, 300, 300);
        assert!(lettering.count() * 100 <= area(&tight) as usize * 45, "the estimate claims most of its box");
        for y in lettering.bounds.y..lettering.bounds.bottom() {
            for x in lettering.bounds.x..lettering.bounds.right() {
                if !lettering.contains(x, y) {
                    continue;
                }
                assert!(tight.contains(x, y), "an estimate reaches outside its box");
                assert_eq!(page.data[y as usize * 300 + x as usize], 20, "an estimate took paper");
                let at = y as usize * 300 + x as usize;
                assert_eq!(owner[at], 0, "two groups claim ({x}, {y})");
                owner[at] = k + 1;
            }
        }
    }
    // Every stroke pixel under a box is claimed exactly once.
    let inked: usize = strokes.iter().map(|r| area(r) as usize).sum();
    assert_eq!(owner.iter().filter(|o| **o != 0).count(), inked);

    // The estimate survives the evidence file, pixel for pixel.
    let blob = g.evidence(None).unwrap();
    let decoded = EvidenceFile::decode(&blob.bytes).unwrap();
    for group in &groups {
        let record = g.record(group, Some(&blob));
        assert_eq!(record.estimated, group.estimated);
        let rebuilt = decoded.lettering(&record).unwrap();
        let digest = format!("{:x}", Sha256::digest(crate::project::buffers::encode_mask(&rebuilt)));
        assert_eq!(digest, record.lettering_sha256, "{}", group.id);
    }
    // And its regions carry it as their seed.
    let median = g.block_median_area();
    for group in &groups {
        let region = g.detector_only_region(group, median);
        assert_eq!(region.group.unwrap().lettering, g.lettering(group));
    }
}

#[test]
fn an_estimate_never_takes_mask_pixels() {
    // CTD's segmentation covers the first caption; the second has none.
    let strokes = [Rect::new(110, 105, 40, 3), Rect::new(110, 115, 40, 3), Rect::new(110, 205, 40, 3)];
    let page = paper(300, 300, &strokes);
    let mut mask = vec![0u8; 300 * 300];
    for stroke in &strokes[..2] {
        for y in stroke.y..stroke.bottom() {
            for x in stroke.x..stroke.right() {
                mask[y as usize * 300 + x as usize] = 255;
            }
        }
    }
    let ctd_boxes = [ctd(100, 100, 60, 30), ctd(100, 200, 60, 15)];
    let mut inputs = Inputs::new(300, 300, Some(&mask));
    inputs.pixel_model = EvidenceModel::Ctd;
    inputs.page = Some(&page);
    inputs.ctd = &ctd_boxes;
    inputs.models = vec![ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole)];
    let g = group(&inputs).unwrap();
    assert_eq!(g.pixel_model, EvidenceModel::Ctd);
    let cleaning: Vec<&TextGroup> = g.cleaning().collect();
    assert_eq!(cleaning.len(), 1);
    assert!(cleaning[0].component_ids.iter().all(|id| id.starts_with("seg-")));
    assert!(!cleaning[0].estimated);
    assert_eq!(g.lettering(cleaning[0]).count(), 240);
    let lone: Vec<&TextGroup> = g.detector_only().collect();
    assert_eq!(lone.len(), 1);
    assert_eq!(lone[0].reasons, vec![ReviewReason::MaskMissingUnderTextBox]);
    let estimate = g.lettering(lone[0]);
    assert!(lone[0].estimated && estimate.count() == 120);
    for y in estimate.bounds.y..estimate.bounds.bottom() {
        for x in estimate.bounds.x..estimate.bounds.right() {
            assert!(!estimate.contains(x, y) || mask[y as usize * 300 + x as usize] == 0);
        }
    }
    let blob = g.evidence(Some(&mask)).unwrap();
    let decoded = EvidenceFile::decode(&blob.bytes).unwrap();
    for group in g.cleaning().chain(g.detector_only()) {
        let record = g.record(group, Some(&blob));
        let rebuilt = decoded.lettering(&record).unwrap();
        assert_eq!(rebuilt.count() as u32, record.lettering_pixels, "{}", group.id);
    }
}

#[test]
fn a_text_box_whose_lettering_another_group_won_folds_into_it() {
    // Two lobes of one balloon that do not overlap enough to be one wall; a
    // CTD box runs down both, and Ogkalu's text box sits in the lower lobe
    // over glyphs the CTD box holds too.
    let mut mask = vec![0u8; (W * H) as usize];
    for y in [290, 320, 350] {
        ring(&mut mask, 190, y, 20);
    }
    let rt_boxes = [
        rt(100, 100, 200, 150, BalloonClass::Bubble, 0.9),
        rt(100, 262, 200, 150, BalloonClass::Bubble, 0.9),
        rt(170, 280, 60, 100, BalloonClass::TextInBubble, 0.9),
    ];
    let ctd_boxes = [ctd(170, 120, 60, 270)];
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.rt = &rt_boxes;
    inputs.ctd = &ctd_boxes;
    inputs.models = models();
    let g = group(&inputs).unwrap();
    let clean: Vec<&TextGroup> = g.groups.iter().filter(|t| t.disposition == Disposition::Clean).collect();
    assert_eq!(clean.len(), 1, "{:?}", g.groups.iter().map(|t| (&t.box_ids, &t.component_ids)).collect::<Vec<_>>());
    assert_eq!(clean[0].box_ids, vec!["ctd-0000".to_owned(), "rt-0002".to_owned()]);
    assert_eq!(clean[0].component_ids.len(), 3);
    assert_eq!(clean[0].origin, GroupOrigin::TextBox);
    assert!(!clean[0].reasons.contains(&ReviewReason::MaskMissingUnderTextBox));
    assert_eq!(g.detector_only().count(), 0, "no second job over the same glyphs");
}

/* ---- balloons, confidence, estimates and splitting (grouping v3) ---- */

/// Two sure balloons side by side, 2 px apart, each with a column of three
/// glyphs.
fn two_balloons() -> (Vec<u8>, [BalloonBox; 2]) {
    let mut mask = vec![0u8; (W * H) as usize];
    for y in [140, 170, 200] {
        ring(&mut mask, 230, y, 24);
        ring(&mut mask, 330, y, 24);
    }
    let walls = [
        rt(100, 100, 200, 150, BalloonClass::Bubble, 0.9),
        rt(302, 100, 200, 150, BalloonClass::Bubble, 0.9),
    ];
    (mask, walls)
}

fn balloon_of_members(g: &Grouping, group: &TextGroup) -> BTreeSet<bool> {
    // true for the right-hand balloon.
    group
        .component_ids
        .iter()
        .map(|id| g.components.iter().find(|c| &c.id == id).unwrap().bounds.x >= 302)
        .collect()
}

#[test]
fn a_ctd_box_straddling_two_balloons_never_takes_the_other_balloons_glyphs() {
    let (mask, walls) = two_balloons();
    // CTD's box runs across both balloons; its centre is in the left one.
    let ctd_boxes = [ctd(215, 130, 150, 100)];
    for right_box in [true, false] {
        let mut rt_boxes = walls.to_vec();
        rt_boxes.push(rt(220, 130, 40, 100, BalloonClass::TextInBubble, 0.9));
        if right_box {
            rt_boxes.push(rt(320, 130, 40, 100, BalloonClass::TextInBubble, 0.9));
        }
        let mut inputs = Inputs::new(W, H, Some(&mask));
        inputs.rt = &rt_boxes;
        inputs.ctd = &ctd_boxes;
        inputs.models = models();
        let g = group(&inputs).unwrap();
        let left = with_box(&g, "rt-0002");
        assert_eq!(left.box_ids, vec!["ctd-0000".to_owned(), "rt-0002".to_owned()]);
        assert_eq!(left.component_ids.len(), 3, "{:?}", left.component_ids);
        assert_eq!(balloon_of_members(&g, left), BTreeSet::from([false]));
        let right = group_at(&g, (331, 141)).unwrap();
        assert_ne!(right.id, left.id);
        assert_eq!(balloon_of_members(&g, right), BTreeSet::from([true]));
        if right_box {
            assert_eq!((right.disposition, right.component_ids.len()), (Disposition::Clean, 3));
        } else {
            // No box of its own balloon: its own job, flagged, never the left
            // balloon's.
            assert_eq!(right.disposition, Disposition::Clean);
            assert!(right.reasons.contains(&ReviewReason::UnassignedComponent), "{:?}", right.reasons);
        }
    }
}

#[test]
fn overlapping_ogkalu_boxes_keep_each_balloons_glyphs() {
    let (mask, walls) = two_balloons();
    // The left text box reaches over the right balloon's column further than
    // the right balloon's own box does.
    let mut rt_boxes = walls.to_vec();
    rt_boxes.push(rt(220, 130, 130, 100, BalloonClass::TextInBubble, 0.9));
    rt_boxes.push(rt(342, 130, 40, 100, BalloonClass::TextInBubble, 0.9));
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.rt = &rt_boxes;
    inputs.models = models();
    let g = group(&inputs).unwrap();
    let left = with_box(&g, "rt-0002");
    let right = with_box(&g, "rt-0003");
    assert_ne!(left.id, right.id);
    assert_eq!(balloon_of_members(&g, left), BTreeSet::from([false]));
    assert_eq!(balloon_of_members(&g, right), BTreeSet::from([true]));
    assert_eq!((left.component_ids.len(), right.component_ids.len()), (3, 3));
}

#[test]
fn an_unconfined_box_across_two_balloons_is_flagged() {
    let (mask, walls) = two_balloons();
    let mut rt_boxes = walls.to_vec();
    rt_boxes.push(rt(220, 130, 150, 100, BalloonClass::TextFree, 0.9));
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.rt = &rt_boxes;
    inputs.models = models();
    let g = group(&inputs).unwrap();
    let straddler = with_box(&g, "rt-0002");
    assert_eq!(straddler.component_ids.len(), 6);
    assert!(straddler.reasons.contains(&ReviewReason::CrossesBalloon), "{:?}", straddler.reasons);
}

#[test]
fn weak_text_boxes_without_lettering_are_candidates() {
    let strokes = [Rect::new(110, 105, 40, 3), Rect::new(110, 115, 40, 3), Rect::new(110, 205, 40, 3)];
    let page = paper(300, 300, &strokes);
    let rt_boxes = [
        rt(100, 100, 60, 30, BalloonClass::TextFree, 0.4),
        rt(100, 200, 60, 15, BalloonClass::TextFree, 0.6),
    ];
    let mut inputs = Inputs::new(300, 300, None);
    inputs.page = Some(&page);
    inputs.rt = &rt_boxes;
    inputs.models = ogkalu_only();
    let g = group(&inputs).unwrap();
    let weak = with_box(&g, "rt-0000");
    assert_eq!((weak.disposition, weak.estimated), (Disposition::Candidate, false));
    assert_eq!(weak.reasons, vec![ReviewReason::MaskMissingUnderTextBox]);
    let sure = with_box(&g, "rt-0001");
    assert_eq!((sure.disposition, sure.estimated), (Disposition::Clean, true));

    // Mask lettering under the weak box confirms it.
    let mut mask = vec![0u8; 300 * 300];
    for stroke in &strokes[..2] {
        for y in stroke.y..stroke.bottom() {
            for x in stroke.x..stroke.right() {
                mask[y as usize * 300 + x as usize] = 255;
            }
        }
    }
    let mut inputs = Inputs::new(300, 300, Some(&mask));
    inputs.page = Some(&page);
    inputs.rt = &rt_boxes;
    inputs.models = models();
    let g = group(&inputs).unwrap();
    assert_eq!(with_box(&g, "rt-0000").disposition, Disposition::Clean);
}

#[test]
fn small_unsure_ctd_blocks_are_candidates() {
    let strokes = [Rect::new(110, 105, 40, 3), Rect::new(110, 115, 40, 3), Rect::new(210, 105, 40, 3)];
    let page = paper(400, 300, &strokes);
    let mut mask = vec![0u8; 400 * 300];
    for y in 20..24 {
        for x in 322..326 {
            mask[y * 400 + x] = 255;
        }
    }
    let block = |x, y, w, h, confidence| DetBox { rect: Rect::new(x, y, w, h), confidence, language: DetectedLanguage::Japanese };
    for (confidence, disposition) in [(0.5, Disposition::Candidate), (0.9, Disposition::Clean)] {
        let ctd_boxes = [
            block(100, 100, 60, 30, 0.9),
            block(200, 100, 60, 30, 0.9),
            block(320, 18, 8, 8, confidence),
            block(330, 200, 8, 8, confidence),
        ];
        let mut inputs = Inputs::new(400, 300, Some(&mask));
        inputs.pixel_model = EvidenceModel::Ctd;
        inputs.page = Some(&page);
        inputs.ctd = &ctd_boxes;
        inputs.models = vec![ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole)];
        let g = group(&inputs).unwrap();
        let speck = with_box(&g, "ctd-0002");
        assert_eq!(speck.disposition, disposition, "{confidence}");
        let bare = with_box(&g, "ctd-0003");
        assert_eq!(bare.disposition, disposition, "{confidence}");
        if disposition == Disposition::Candidate {
            assert_eq!(speck.reasons, vec![ReviewReason::IsolatedComponent]);
            assert_eq!(bare.reasons, vec![ReviewReason::MaskMissingUnderTextBox]);
        }
    }
}

#[test]
fn specks_beside_a_layout_group_attach_to_it() {
    let mut mask = vec![0u8; (W * H) as usize];
    for y in [100, 128, 156, 184] {
        ring(&mut mask, 300, y, 24);
        ring(&mut mask, 268, y, 24);
    }
    // An ellipsis under the right column: three dots under the speck floor.
    for y in [212, 220, 228] {
        fill(&mut mask, Rect::new(310, y, 3, 3));
    }
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.models = vec![ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole)];
    let g = group(&inputs).unwrap();
    let clean: Vec<&TextGroup> = g.cleaning().collect();
    assert_eq!(clean.len(), 1);
    assert_eq!(clean[0].component_ids.len(), 8 + 3, "the ellipsis is the block's");
    assert!(g.components.iter().all(|c| c.group.is_some()));
}

#[test]
fn the_layout_sweep_finds_the_same_blocks_as_every_pair() {
    // A deterministic scatter of glyphs, some touching, some far apart.
    let mut mask = vec![0u8; (W * H) as usize];
    let mut seed = 7u64;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % bound
    };
    for _ in 0..400 {
        let (x, y, s) = (next(1200) as i64 + 20, next(1000) as i64 + 20, next(20) as u32 + 8);
        fill(&mut mask, Rect::new(x, y, s, s));
    }
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.models = vec![ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole)];
    let g = group(&inputs).unwrap();
    let loose: Vec<usize> = (0..g.components.len()).filter(|&i| g.components[i].pixels >= SPECK_PIXELS).collect();
    let mut every = Partition::new(loose.len());
    for a in 0..loose.len() {
        for b in a + 1..loose.len() {
            if adjacent(&g.components[loose[a]].bounds, &g.components[loose[b]].bounds) {
                every.union(a, b);
            }
        }
    }
    let expected: Vec<BTreeSet<String>> = every
        .classes()
        .into_iter()
        .map(|class| class.iter().map(|&k| g.components[loose[k]].id.clone()).collect())
        .collect();
    let found: Vec<BTreeSet<String>> = g
        .groups
        .iter()
        .map(|group| {
            group
                .component_ids
                .iter()
                .filter(|id| g.components.iter().any(|c| &&c.id == id && c.pixels >= SPECK_PIXELS))
                .cloned()
                .collect()
        })
        .collect();
    // Reach may join blocks after layout, never split one: each block every
    // pair would find lies in one group, and each group is whole blocks.
    for block in &expected {
        assert_eq!(found.iter().filter(|group| !group.is_disjoint(block)).count(), 1, "{block:?} was split");
    }
    for group in &found {
        let covered: BTreeSet<String> =
            expected.iter().filter(|block| block.is_subset(group)).flatten().cloned().collect();
        assert_eq!(&covered, group, "a group holds part of a block");
    }
}

#[test]
fn group_ids_are_unique_and_scoped_to_their_page() {
    let f = fixture();
    let first = run(&f, &f.rt, &f.ctd);
    let ids: BTreeSet<&String> = first.groups.iter().map(|g| &g.id).collect();
    assert_eq!(ids.len(), first.groups.len());
    let mut inputs = Inputs::new(W, H, Some(&f.mask));
    inputs.rt = &f.rt;
    inputs.ctd = &f.ctd;
    inputs.models = models();
    inputs.seams = vec![Seam::Vertical(1024), Seam::Horizontal(1024)];
    inputs.scope = "0123abcd|page-2".into();
    let other_page = group(&inputs).unwrap();
    assert_eq!(other_page.groups.len(), first.groups.len());
    assert!(other_page.groups.iter().all(|g| !ids.contains(&g.id)), "an id repeats across pages");
}

#[test]
fn a_box_over_a_speck_is_estimated_with_the_speck_kept() {
    let strokes = [Rect::new(110, 105, 40, 3), Rect::new(110, 115, 40, 3), Rect::new(110, 125, 40, 3)];
    let page = paper(300, 300, &strokes);
    // The mask saw one dot of the caption, a mark beside its strokes.
    let mut mask = vec![0u8; 300 * 300];
    for y in 120..122 {
        for x in 152..154 {
            mask[y * 300 + x] = 255;
        }
    }
    let ctd_boxes = [ctd(100, 100, 60, 30)];
    let mut inputs = Inputs::new(300, 300, Some(&mask));
    inputs.pixel_model = EvidenceModel::Ctd;
    inputs.page = Some(&page);
    inputs.ctd = &ctd_boxes;
    inputs.models = vec![ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole)];
    let g = group(&inputs).unwrap();
    let caption = with_box(&g, "ctd-0000");
    assert_eq!(caption.component_ids.len(), 1);
    assert!(caption.estimated);
    let lettering = g.lettering(caption);
    assert_eq!(lettering.count() as u32, caption.lettering_pixels);
    assert_eq!(caption.lettering_pixels, 4 + 3 * 40 * 3);
    assert!(lettering.contains(152, 120) && lettering.contains(110, 105));
    let blob = g.evidence(Some(&mask)).unwrap();
    let decoded = EvidenceFile::decode(&blob.bytes).unwrap();
    let record = g.record(caption, Some(&blob));
    let rebuilt = decoded.lettering(&record).unwrap();
    let digest = format!("{:x}", Sha256::digest(crate::project::buffers::encode_mask(&rebuilt)));
    assert_eq!(digest, record.lettering_sha256);
}

#[test]
fn a_duplicate_box_never_cleans_the_halo_its_twin_left() {
    // One caption, two boxes: the second is shifted right, overlapping the
    // first by exactly half, and holds a thin outline the first does not.
    let strokes = [
        Rect::new(110, 105, 50, 3),
        Rect::new(110, 115, 50, 3),
        Rect::new(110, 125, 50, 3),
        Rect::new(185, 100, 1, 30),
    ];
    let page = paper(300, 300, &strokes);
    let rt_boxes = [
        rt(100, 100, 60, 30, BalloonClass::TextFree, 0.9),
        rt(130, 100, 60, 30, BalloonClass::TextFree, 0.9),
    ];
    let mut inputs = Inputs::new(300, 300, None);
    inputs.page = Some(&page);
    inputs.rt = &rt_boxes;
    inputs.models = ogkalu_only();
    let g = group(&inputs).unwrap();
    let groups: Vec<&TextGroup> = g.detector_only().collect();
    assert_eq!(groups.len(), 1, "{:?}", g.groups.iter().map(|t| (&t.box_ids, t.lettering_pixels)).collect::<Vec<_>>());
    assert_eq!(groups[0].box_ids, vec!["rt-0000".to_owned(), "rt-0001".to_owned()]);
    let lettering = g.lettering(groups[0]);
    assert!(!lettering.contains(185, 110), "the outline was cleaned");

    // Twin boxes over one caption, placed in different balloons, are one.
    let rt_boxes = [
        rt(90, 90, 70, 50, BalloonClass::Bubble, 0.9),
        rt(100, 100, 60, 30, BalloonClass::TextFree, 0.9),
        rt(100, 100, 60, 30, BalloonClass::TextInBubble, 0.8),
    ];
    let mut inputs = Inputs::new(300, 300, None);
    inputs.page = Some(&page);
    inputs.rt = &rt_boxes;
    inputs.models = ogkalu_only();
    let g = group(&inputs).unwrap();
    let groups: Vec<&TextGroup> = g.groups.iter().filter(|t| !t.box_ids.is_empty()).collect();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].box_ids, vec!["rt-0001".to_owned(), "rt-0002".to_owned()]);
}

#[test]
fn a_group_larger_than_the_limit_on_both_axes_splits_on_both() {
    let (width, height) = (1600u32, 1200u32);
    let mut mask = vec![0u8; (width * height) as usize];
    for y in (60..1150).step_by(30) {
        for x in (60..1550).step_by(30) {
            for yy in y..y + 20 {
                for xx in x..x + 20 {
                    mask[yy as usize * width as usize + xx as usize] = 255;
                }
            }
        }
    }
    let boxes = [rt(50, 50, 1510, 1110, BalloonClass::TextFree, 0.9)];
    let mut inputs = Inputs::new(width, height, Some(&mask));
    inputs.rt = &boxes;
    let g = group(&inputs).unwrap();
    let parts: Vec<&TextGroup> = g.cleaning().collect();
    assert!(parts.len() >= 4, "{}", parts.len());
    let parent = parts[0].split.as_ref().unwrap().parent.clone();
    assert!(parent.starts_with("tg-"));
    let mut all = BTreeSet::new();
    for part in &parts {
        assert!(part.bounds.w <= MAX_GROUP_SIDE && part.bounds.h <= MAX_GROUP_SIDE, "{:?}", part.bounds);
        let split = part.split.as_ref().unwrap();
        assert_eq!((split.parent.as_str(), split.of as usize), (parent.as_str(), parts.len()));
        assert_eq!(part.id, format!("{parent}-p{}", split.index));
        assert_eq!(g.record(part, None).split.as_ref().map(|s| s.parent.as_str()), Some(parent.as_str()));
        for id in &part.component_ids {
            assert!(all.insert(id.clone()));
        }
    }
    assert_eq!(all.len(), g.components.len());
}

/* ---- grouping v4: folds follow balloons, ids digest exact pixels ---- */

#[test]
fn pixel_less_boxes_in_two_balloons_stay_two_groups() {
    // Two sure balloons 2 px apart. The left balloon's text box reaches over
    // the whole of the right balloon's (far more than half the smaller box),
    // and no mask pixel lies under either.
    let strokes = [
        Rect::new(210, 140, 80, 3),
        Rect::new(210, 150, 80, 3),
        Rect::new(210, 160, 80, 3),
        Rect::new(315, 140, 25, 3),
        Rect::new(315, 150, 25, 3),
        Rect::new(315, 160, 25, 3),
    ];
    let page = paper(600, 300, &strokes);
    let walls = [
        rt(100, 100, 200, 150, BalloonClass::Bubble, 0.9),
        rt(302, 100, 200, 150, BalloonClass::Bubble, 0.9),
    ];
    let left_box = rt(200, 130, 150, 60, BalloonClass::TextInBubble, 0.9);
    let right_box = rt(310, 135, 36, 50, BalloonClass::TextInBubble, 0.9);
    // The same two boxes directly, and through a free caption box over both
    // (each overlaps it wholly): a chain of folds is refused as a fold is.
    let direct = [walls[0].clone(), walls[1].clone(), left_box.clone(), right_box.clone()];
    let left_inner = rt(205, 135, 90, 50, BalloonClass::TextInBubble, 0.9);
    let chained = [walls[0].clone(), walls[1].clone(), left_inner, right_box.clone(), left_box.clone()];
    let chained = {
        let mut boxes = chained.to_vec();
        boxes[4].class = BalloonClass::TextFree;
        boxes
    };
    let blank = vec![0u8; 600 * 300];
    for rt_boxes in [&direct[..], &chained[..]] {
        // With a lettering mask that has nothing there, and with no mask at
        // all: each box then takes its bounded ink estimate.
        for mask in [Some(&blank[..]), None] {
            let mut inputs = Inputs::new(600, 300, mask);
            inputs.page = Some(&page);
            inputs.rt = rt_boxes;
            inputs.models = if mask.is_some() { models() } else { ogkalu_only() };
            let g = group(&inputs).unwrap();
            let left = with_box(&g, "rt-0002");
            let right = with_box(&g, "rt-0003");
            let shape = g.groups.iter().map(|t| (&t.box_ids, t.lettering_pixels, &t.reasons)).collect::<Vec<_>>();
            assert_ne!(left.id, right.id, "two balloons' boxes are one group: {shape:?}");
            for (group, right_side) in [(left, false), (right, true)] {
                assert_eq!(group.disposition, Disposition::Clean);
                let lettering = g.lettering(group);
                assert_eq!(lettering.count(), 3 * if right_side { 25 } else { 80 } * 3, "{shape:?}");
                for y in lettering.bounds.y..lettering.bounds.bottom() {
                    for x in lettering.bounds.x..lettering.bounds.right() {
                        assert!(!lettering.contains(x, y) || (x >= 302) == right_side, "({x}, {y}) {shape:?}");
                    }
                }
                assert!(!group.reasons.contains(&ReviewReason::CrossesBalloon), "{shape:?}");
            }
        }
    }
}

#[test]
fn an_estimate_across_two_balloons_is_flagged() {
    // An unconfined caption box over two balloons' strokes, no pixel model:
    // its estimate takes both, and says so.
    let strokes = [Rect::new(210, 140, 80, 3), Rect::new(210, 150, 80, 3), Rect::new(315, 140, 25, 3)];
    let page = paper(600, 300, &strokes);
    let rt_boxes = [
        rt(100, 100, 200, 150, BalloonClass::Bubble, 0.9),
        rt(302, 100, 200, 150, BalloonClass::Bubble, 0.9),
        rt(200, 130, 150, 40, BalloonClass::TextFree, 0.9),
    ];
    let mut inputs = Inputs::new(600, 300, None);
    inputs.page = Some(&page);
    inputs.rt = &rt_boxes;
    inputs.models = ogkalu_only();
    let g = group(&inputs).unwrap();
    let caption = with_box(&g, "rt-0002");
    assert_eq!(g.lettering(caption).count(), 3 * 80 * 2 + 3 * 25);
    assert!(caption.reasons.contains(&ReviewReason::CrossesBalloon), "{:?}", caption.reasons);
}

#[test]
fn group_ids_digest_exact_pixels_not_bounds_and_counts() {
    // One column of three glyphs, drawn as an L or as its mirror: the same
    // bounds and pixel counts per glyph, different pixels.
    fn column(mirrored: bool) -> Vec<u8> {
        let mut mask = vec![0u8; (W * H) as usize];
        for y in [100, 130, 160] {
            fill(&mut mask, Rect::new(if mirrored { 118 } else { 100 }, y, 6, 24));
            fill(&mut mask, Rect::new(100, y + 18, 24, 6));
        }
        mask
    }
    let named = |mask: &[u8], origin: (i64, i64)| {
        let mut inputs = Inputs::new(W, H, Some(mask));
        inputs.models = vec![ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole)];
        inputs.scope = "0123abcd|page-1".into();
        inputs.origin = origin;
        let g = group(&inputs).unwrap();
        let clean: Vec<&TextGroup> = g.cleaning().collect();
        assert_eq!(clean.len(), 1);
        let components: Vec<(Rect, u32)> = g.components.iter().map(|c| (c.bounds, c.pixels)).collect();
        (clean[0].id.clone(), components)
    };
    let (plain, plain_components) = named(&column(false), (0, 0));
    let (mirrored, mirrored_components) = named(&column(true), (0, 0));
    assert_eq!(plain_components, mirrored_components, "the fixture's point: bounds and counts agree");
    assert_ne!(plain, mirrored, "different glyphs share an id");
    assert_eq!(named(&column(false), (0, 0)).0, plain, "the same evidence names the same group again");

    // The same pixels seen from a crop placed 40 px down keep the id.
    let shifted: Vec<u8> = column(false).split_off(40 * W as usize).into_iter().chain(vec![0u8; 40 * W as usize]).collect();
    assert_eq!(named(&shifted, (0, 40)).0, plain, "a crop of the same evidence renamed the group");
}

const EVERY_MODEL: [EvidenceModel; 4] =
    [EvidenceModel::SamTsL, EvidenceModel::Ctd, EvidenceModel::OgkaluFull, EvidenceModel::OgkaluSmall];
const EVERY_SPATIAL: [SpatialInput; 4] =
    [SpatialInput::Whole, SpatialInput::Halves, SpatialInput::CloudTiles, SpatialInput::OverlappingCloudTiles];

/// Diagnostics name SAM-TS-L by its own checkpoint and pinned revision, and
/// never name Koharu's layout detector or Manga Text Segmentation 2025, which
/// this application does not run, nor call any model "Koharu" but SAM-TS-L's
/// published repository.
#[test]
fn model_descriptions_name_exact_checkpoints() {
    let sam = ModelUse::new(EvidenceModel::SamTsL, Execution::Cloud, SpatialInput::OverlappingCloudTiles);
    assert_eq!(
        sam.identity(),
        "SAM-TS-L lettering mask, mayocream/koharu-text-sam-ts-l @ 5dd97423e0fbf2404264979136d47e8101144046 \
         [cloud, cloud tiles 1.1.0: 1024 tiles overlapping by at least 256, stitched by core ownership]"
    );
    assert!(sam.describe().starts_with(&sam.identity()));
    assert!(sam.describe().ends_with("longest side resized to 1024 (bilinear), top-left on a gray-128 1024 square; logits > 0; nearest restore to source size"));
    let local = ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole);
    assert!(local.identity().ends_with("[local, whole page (or one long-strip segment crop) in one inference]"));
    let full = ModelUse::new(EvidenceModel::OgkaluFull, Execution::Local, SpatialInput::Halves);
    assert_eq!(
        full.identity(),
        "Ogkalu comic text & bubble detector - Full, ogkalu/comic-text-and-bubble-detector detector.onnx (RT-DETR v2, FP32) \
         @ 16e8a622f91fabc6b5b65c96d32d1183f8843546 [local, two horizontal halves]"
    );
    let small = ModelUse::new(EvidenceModel::OgkaluSmall, Execution::Local, SpatialInput::Whole);
    assert!(small.identity().contains("detector-v4-s_int8.onnx (INT8) @ revision not pinned [local,"), "{}", small.identity());
    assert!(ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole)
        .identity()
        .starts_with("Comic Text Detector (CTD), zyddnys/manga-image-translator comictextdetector.pt.onnx @ release beta-0.2.1"));
    assert!(ModelUse::new(EvidenceModel::SamTsL, Execution::Cloud, SpatialInput::CloudTiles)
        .identity()
        .contains("[cloud, cloud tiles 1.0.0: non-overlapping tiles of at most 1024]"));

    for model in EVERY_MODEL {
        for execution in [Execution::Local, Execution::Cloud] {
            for spatial in EVERY_SPATIAL {
                let use_ = ModelUse::new(model, execution, spatial);
                let text = serde_json::to_string(&use_.description()).unwrap();
                assert!(text.contains(model.name()) && text.contains(model.checkpoint()) && text.contains(spatial.label()));
                assert!(!text.contains("koharu-layout-rfdetr-seg-2xl-1152") && !text.contains("manga-text-segmentation"));
                assert!(!text.replace(SAM_TS_L_REPOSITORY, "").to_lowercase().contains("koharu"), "{text}");
                assert_eq!(text.contains(SAM_TS_L_REPOSITORY), model == EvidenceModel::SamTsL, "{text}");
            }
        }
    }
}

/// A patch's `params_snapshot["group"]` spells out every model the group's
/// evidence came from, beside the bare tags it always carried.
#[test]
fn a_group_snapshot_describes_its_models() {
    let f = fixture();
    let g = run(&f, &f.rt, &f.ctd);
    let a = with_box(&g, "rt-0001");
    let snapshot = g.record(a, None).snapshot();
    assert_eq!(snapshot["models"][0], serde_json::json!({
        "model": "samTsL", "execution": "cloud", "spatial": "overlappingCloudTiles" }));
    let described: Vec<&str> = snapshot["modelsDescribed"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(described.len(), snapshot["models"].as_array().unwrap().len());
    assert_eq!(described[0], models()[0].identity());
    assert!(described[0].contains("mayocream/koharu-text-sam-ts-l @ 5dd97423e0fbf2404264979136d47e8101144046 [cloud, cloud tiles 1.1.0"));
    assert!(described.iter().any(|d| d.contains("ogkalu/comic-text-and-bubble-detector detector.onnx (RT-DETR v2, FP32) @ 16e8a622f91fabc6b5b65c96d32d1183f8843546")));
}

/// Big effect lettering: SAM pixels and text-free boxes over them, as Ogkalu
/// boxes an effect it cut in pieces.
fn effect(glyphs: &[Rect], boxes: &[Rect]) -> Grouping {
    let mut mask = vec![0u8; (W * H) as usize];
    for &glyph in glyphs {
        fill(&mut mask, glyph);
    }
    let rt: Vec<BalloonBox> = boxes.iter().map(|b| rt(b.x, b.y, b.w, b.h, BalloonClass::TextFree, 0.8)).collect();
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.rt = &rt;
    inputs.models = vec![
        ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole),
        ModelUse::new(EvidenceModel::OgkaluFull, Execution::Local, SpatialInput::Whole),
    ];
    group(&inputs).unwrap()
}

#[test]
fn an_effect_the_boxes_cut_in_two_is_one_group() {
    let glyphs: Vec<Rect> = (0..4).map(|k| Rect::new(100 + 80 * k, 600, 60, 60)).collect();
    // Each box holds two glyphs; the gap between the halves is one glyph gap.
    let g = effect(&glyphs, &[Rect::new(95, 595, 150, 70), Rect::new(255, 595, 150, 70)]);
    let clean: Vec<&TextGroup> = g.cleaning().collect();
    assert_eq!(clean.len(), 1, "one effect, one job");
    assert_eq!(clean[0].component_ids.len(), 4);
    assert_eq!(clean[0].box_ids.len(), 2);
    assert!(clean[0].reasons.is_empty());
}

#[test]
fn a_stroke_no_box_claimed_is_cleaned_with_the_effect_beside_it() {
    let mut glyphs: Vec<Rect> = (0..3).map(|k| Rect::new(100 + 80 * k, 600, 60, 60)).collect();
    // A long stroke past the box, too big to be punctuation: before reach it
    // was a candidate row, left on the page.
    glyphs.push(Rect::new(345, 560, 20, 140));
    let g = effect(&glyphs, &[Rect::new(95, 595, 230, 70)]);
    assert_eq!(g.candidates().count(), 0);
    let clean: Vec<&TextGroup> = g.cleaning().collect();
    assert_eq!(clean.len(), 1);
    assert_eq!(clean[0].component_ids.len(), 4);
    assert!(clean[0].reasons.is_empty(), "claimed now: {:?}", clean[0].reasons);
    let stroke = g.components.iter().find(|c| c.bounds.x == 345).unwrap();
    assert_eq!(stroke.via, Some(Assignment::Proximity));
    assert!(stroke.reasons.is_empty());
}

#[test]
fn text_out_of_reach_stays_its_own_group() {
    let mut glyphs: Vec<Rect> = (0..2).map(|k| Rect::new(100 + 80 * k, 600, 60, 60)).collect();
    glyphs.extend((0..2).map(|k| Rect::new(400 + 80 * k, 600, 60, 60)));
    let g = effect(&glyphs, &[Rect::new(95, 595, 150, 70), Rect::new(395, 595, 150, 70)]);
    assert_eq!(g.cleaning().count(), 2, "a 160 px gap is past the {REACH_MAX} px reach");
}

#[test]
fn an_effect_no_box_saw_is_cleaned_whole() {
    // Ogkalu boxed only the right-hand glyph. The two unboxed pieces beside
    // each other are one job, and outside every balloon.
    let glyphs = [Rect::new(100, 600, 60, 60), Rect::new(185, 560, 20, 140), Rect::new(600, 600, 60, 60)];
    let g = effect(&glyphs, &[Rect::new(595, 595, 70, 70)]);
    assert_eq!(g.candidates().count(), 0);
    let unboxed = group_at(&g, (101, 601)).unwrap();
    assert_eq!(unboxed.component_ids.len(), 2);
    assert_eq!((unboxed.bubble.clone(), unboxed.reasons.clone()), (None, vec![ReviewReason::UnassignedComponent]));
    assert_eq!(g.record(unboxed, None).review_key(), None);
    assert_eq!(g.cleaning().count(), 2);
}

#[test]
fn bubble_text_with_no_balloon_box_keeps_its_own_centre() {
    // Two text_bubble boxes and no bubble box: joined, their centre would lie
    // in neither box and read as outside text.
    let mut mask = vec![0u8; (W * H) as usize];
    fill(&mut mask, Rect::new(100, 600, 20, 20));
    fill(&mut mask, Rect::new(130, 600, 20, 20));
    let rt = [
        rt(98, 598, 24, 24, BalloonClass::TextInBubble, 0.9),
        rt(128, 598, 24, 24, BalloonClass::TextInBubble, 0.9),
    ];
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.rt = &rt;
    inputs.models = models();
    let g = group(&inputs).unwrap();
    assert_eq!(g.cleaning().count(), 2);
}

#[test]
fn reach_never_builds_a_group_taller_than_the_detection_overlap() {
    // A column of glyphs 30 px apart, each under its own text_free box, runs
    // 920 px: one group would be taller than a long strip's overlap, yet
    // under the split limit that would cut it anyway.
    let glyphs: Vec<Rect> = (0..31).map(|k| Rect::new(600, 10 + 30 * k, 20, 20)).collect();
    let boxes: Vec<Rect> = glyphs.iter().map(|g| Rect::new(g.x - 2, g.y - 2, 24, 24)).collect();
    let g = effect(&glyphs, &boxes);
    let clean: Vec<&TextGroup> = g.cleaning().collect();
    assert!(clean.len() >= 2);
    for group in &clean {
        assert!(group.bounds.h <= crate::strip::DETECTION_OVERLAP, "{:?}", group.bounds);
    }
    assert_eq!(clean.iter().map(|group| group.component_ids.len()).sum::<usize>(), 31);
}

#[test]
fn a_speckle_box_never_takes_the_lettering_beside_it_out_of_cleaning() {
    // A tiny, unsure CTD block over a 4 px mark is speckle and is listed. A
    // real glyph beside it stays a cleaning job of its own.
    let mut mask = vec![0u8; (W * H) as usize];
    fill(&mut mask, Rect::new(402, 402, 4, 4));
    fill(&mut mask, Rect::new(415, 395, 20, 20));
    for k in 0..4 {
        fill(&mut mask, Rect::new(100 + 60 * k, 900, 40, 40));
    }
    let ctd_boxes: Vec<DetBox> = std::iter::once(DetBox {
        rect: Rect::new(400, 400, 8, 8),
        confidence: 0.5,
        language: DetectedLanguage::Japanese,
    })
    .chain((0..4).map(|k| ctd(98 + 60 * k, 898, 44, 44)))
    .collect();
    let mut inputs = Inputs::new(W, H, Some(&mask));
    inputs.ctd = &ctd_boxes;
    inputs.models = models();
    let g = group(&inputs).unwrap();
    let glyph = group_at(&g, (420, 400)).unwrap();
    assert_eq!(glyph.disposition, Disposition::Clean);
    let speck = group_at(&g, (403, 403)).unwrap();
    assert_ne!(speck.id, glyph.id);
    assert_eq!(speck.disposition, Disposition::Candidate);
}
