//! The balloon question on a real page, one line per region.
//!
//! ```text
//! spike-gate-probe [--models DIR] [--outside review|clean] <page.png> ...
//! ```
//!
//! For every region the detector emits: the balloon detector's answer and the
//! boxes it rests on, the paper reading (`balloon::interior_of`) around the
//! lettering, the combined answer the run uses (`balloon::in_bubble`: the
//! detector, or solid paper where the detector said outside), and the gate's
//! verdict. The point is to read a region that landed in review under *"text
//! outside a speech bubble"* back to whichever of the two opinions put it
//! there.
//!
//! The region list is the run's list for the default models (CTD with the
//! small Ogkalu detector): the text groups `text_groups::group` makes of CTD's
//! segmentation and both detectors' boxes. An `ADOPTED` line marks each group
//! with no segmentation under it and reports its bounded ink estimate, the
//! seed the run cleans it from.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use cleaner_core::accel::Preference;
use cleaner_core::balloon::{self, BalloonDetector};
use cleaner_core::detect::{Detector, MASK_THRESHOLD, Region};
use cleaner_core::text_groups::{self, EvidenceModel, Execution, ModelUse, SpatialInput};
use cleaner_core::gate::{OutsideText, ScriptGate, Verdict};
use cleaner_core::image::decode;

const DETECTOR: &str = "comictextdetector.onnx";
const BALLOONS: &str = "comic-text-and-bubble-detector-detector-v4-s_int8.onnx";
const GATE_MODEL: &str = "image-script-identification-osd_lstm.onnx";
const GATE_LABELS: &str = "image-script-identification-osd_labels.json";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut models = PathBuf::from("models");
    let mut lama_out: Option<PathBuf> = None;
    let mut outside = OutsideText::Review;
    let mut pages: Vec<PathBuf> = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--models" => models = PathBuf::from(args.next().context("--models needs a path")?),
            "--lama" => lama_out = Some(PathBuf::from(args.next().context("--lama needs a dir")?)),
            // §3's opt-in, so the probe can show what a region the balloon
            // question puts outside a bubble would do under either setting.
            "--outside" => outside = OutsideText::from_arg(args.next().as_deref()),
            other => pages.push(PathBuf::from(other)),
        }
    }
    if pages.is_empty() {
        return Err(anyhow!(
            "usage: spike-gate-probe [--models DIR] [--lama OUTDIR] [--outside review|clean] <page.png> ..."
        ));
    }

    cleaner_core::runtime::load(&cleaner_core::runtime::find(None).context("ONNX Runtime")?)?;
    let preference = Preference::Automatic;
    let mut detector = Detector::open(&models.join(DETECTOR), preference)?;
    let mut balloons = BalloonDetector::open(&models.join(BALLOONS), preference)?;
    let mut gate = ScriptGate::open(&models.join(GATE_MODEL), &models.join(GATE_LABELS), preference)?;

    for path in pages {
        let bytes = std::fs::read(&path)?;
        let page = decode(&bytes).map_err(|e| anyhow!("{e}"))?;
        let mut detection = detector.detect(&page)?;
        let boxes = balloons.detect(&page)?;
        // The text groups, exactly as `run.rs` makes them for these models.
        let pixels: Vec<u8> = detection.segmentation.levels.iter()
            .map(|&v| if v >= MASK_THRESHOLD { 255 } else { 0 })
            .collect();
        let mut inputs = text_groups::Inputs::new(page.width, page.height, Some(&pixels));
        inputs.pixel_model = EvidenceModel::Ctd;
        inputs.page = Some(&page);
        inputs.rt = &boxes;
        inputs.ctd = &detection.boxes;
        inputs.models = vec![
            ModelUse::new(EvidenceModel::OgkaluSmall, Execution::Local, SpatialInput::Whole),
            ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole),
        ];
        let grouping = text_groups::group(&inputs).map_err(|e| anyhow!("{e}"))?;
        let median = grouping.block_median_area();
        let mut regions: Vec<Region> = grouping.cleaning().map(|group| grouping.region(group, median)).collect();
        let detected_count = regions.len();
        let adopted: Vec<Region> =
            grouping.detector_only().map(|group| grouping.detector_only_region(group, median)).collect();
        let marks: Vec<(cleaner_core::mask::Rect, f32)> =
            adopted.iter().map(|r| (r.masking, r.members[0].confidence)).collect();
        grouping.paint_estimates(&mut detection.segmentation);
        regions.extend(adopted);
        cleaner_core::detect::sort_regions(&mut regions);
        println!(
            "PAGE {} {}x{} regions={} detected={} adopted={} balloons={}",
            path.display(),
            page.width,
            page.height,
            regions.len(),
            detected_count,
            marks.len(),
            boxes.len()
        );
        for b in &boxes {
            println!("BALLOON {:?} {},{},{}x{} @{:.2}", b.class, b.rect.x, b.rect.y, b.rect.w, b.rect.h, b.score);
        }
        for b in &detection.boxes {
            println!("RAW {:?} {},{},{}x{} @{:.2}", b.language, b.rect.x, b.rect.y, b.rect.w, b.rect.h, b.confidence);
        }
        // The adopted regions, by their index in the sorted list - which is the
        // index a run would name them by - and the ink estimate each seeds
        // from. An empty seed is listed by the run, never cleaned.
        for (i, region) in regions.iter().enumerate() {
            let Some((_, score)) = marks.iter().find(|(rect, _)| *rect == region.masking) else {
                continue;
            };
            let m = region.masking;
            let cx = m.x + m.w as i64 / 2;
            let cy = m.y + m.h as i64 / 2;
            let from = boxes
                .iter()
                .filter(|b| b.rect.contains(cx, cy) && (b.score - score).abs() < 1e-6)
                .map(|b| format!("{:?}@{:.2}", b.class, b.score))
                .next()
                .unwrap_or_else(|| format!("?@{score:.2}"));
            let seed = region.group.as_ref().map_or(0, |held| held.lettering.count());
            println!(
                "ADOPTED {i} box={},{},{}x{} from={from} seed_pixels={} large={}",
                m.x,
                m.y,
                m.w,
                m.h,
                seed,
                region.flagged_large
            );
        }
        for (i, region) in regions.iter().enumerate() {
            let m = region.masking;
            let cx = m.x + m.w as i64 / 2;
            let cy = m.y + m.h as i64 / 2;
            let over: Vec<String> = boxes
                .iter()
                .filter(|b| b.rect.contains(cx, cy))
                .map(|b| format!("{:?}@{:.2}", b.class, b.score))
                .collect();
            let detected = balloon::detected(m, &boxes);
            let inside = detected.inside();
            let text = region.text_bounds();
            let interior = balloon::interior_of(&page, &detection.segmentation, text);
            let combined = balloon::in_bubble(&page, &detection.segmentation, text, m, &boxes);
            let verdict = gate.judge(&page, &detection.segmentation, region, detected, combined, outside)?;
            let verdict = match &verdict {
                Verdict::Clean { script } => format!("Clean({script})"),
                other => format!("{other:?}"),
            };
            println!(
                "REGION {i} box={},{},{}x{} text={},{},{}x{} detector={inside} over=[{}] paper={interior:?} inside={combined} verdict={verdict}",
                m.x, m.y, m.w, m.h, text.x, text.y, text.w, text.h, over.join(" ")
            );
        }

        if let Some(out) = lama_out.as_ref() {
            lama_holes(&models, &path, &page, &detection, &regions, out)?;
        }
    }
    Ok(())
}

/// The question the retry button answers by accident: rung 2 with the hole it
/// is given on a first pass (`Fitted::ink`) against the hole a re-run gives it
/// (the stored applied mask, which is `ink ⊕ ISOLATION_RADIUS`). Three crops
/// per region - the page, the first-pass result, the re-run's - so the two can
/// be looked at rather than argued about.
fn lama_holes(
    models: &std::path::Path,
    path: &std::path::Path,
    page: &cleaner_core::image::Raster,
    detection: &cleaner_core::detect::Detection,
    regions: &[cleaner_core::detect::Region],
    out: &std::path::Path,
) -> Result<()> {
    use cleaner_core::engines::lama::Inpainter;
    use cleaner_core::engines::model::page_crop;
    use cleaner_core::fit;
    use cleaner_core::image::{Format, encode};

    std::fs::create_dir_all(out)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut lama = Inpainter::open(&models.join("lama-manga.onnx"), Preference::Automatic)?;
    let scale = detection.segmentation.proxy_scale();
    let noise = fit::page_noise_sigma(page);
    let edges = fit::EdgeMap::sobel(page);
    let isolation = cleaner_core::constants::ISOLATION_RADIUS;

    for (i, region) in regions.iter().enumerate() {
        // A group seeds from its own lettering, as the run's regions do.
        let Some(seed) = region.group.as_ref().map(|held| held.lettering.clone()) else { continue };
        if seed.is_empty() {
            continue;
        }
        let first = fit::fit(page, &seed, scale, noise, &edges, false);
        let wider_ink = first.ink.dilated(isolation, page.width, page.height);
        let retry = fit::Fitted { mask: wider_ink.clone(), ink: wider_ink, ..first.clone() };

        let a = match lama.render(page, &first) {
            Ok(r) => r,
            Err(e) => {
                println!("LAMA {i} first-pass refused: {e}");
                continue;
            }
        };
        let b = lama.render(page, &retry)?;
        let view = b.mask.bounds.grown(24, page.width, page.height);
        let before = page_crop(page, view);
        let paste = |r: &cleaner_core::engines::model::Rendered| {
            let mut shown = page_crop(page, view);
            for y in r.mask.bounds.y..r.mask.bounds.bottom() {
                for x in r.mask.bounds.x..r.mask.bounds.right() {
                    if !r.mask.contains(x, y) || !view.contains(x, y) {
                        continue;
                    }
                    let (sx, sy) = ((x - r.mask.bounds.x) as u32, (y - r.mask.bounds.y) as u32);
                    let (dx, dy) = ((x - view.x) as u32, (y - view.y) as u32);
                    for c in 0..page.mode.samples() {
                        shown.set_sample(dx, dy, c, r.pixels.sample(sx, sy, c));
                    }
                }
            }
            shown
        };
        for (tag, raster) in [("before", before), ("first", paste(&a)), ("retry", paste(&b))] {
            let bytes = encode(&raster, Format::Png).map_err(|e| anyhow!("{e}"))?;
            std::fs::write(out.join(format!("{stem}-r{i}-{tag}.png")), bytes)?;
        }
        println!(
            "LAMA {i} ink={} first_hole={} retry_hole={} first_written={} retry_written={}",
            first.ink.count(),
            first.ink.count(),
            retry.ink.count(),
            a.mask.count(),
            b.mask.count()
        );
    }
    Ok(())
}
