//! The gate's rescue reader, with and without, one line per region.
//!
//! ```text
//! spike-ocr-probe [--models DIR] [--all] [--time] <page.png> ...
//! ```
//!
//! Runs the whole front end a page goes through - the text detector, the
//! balloon detector, the region build (`text_groups::group`, as a run with
//! the default models makes it) - and then judges every region **twice**:
//! once with a bare [`ScriptGate`] and once with one the reader is attached to.
//! A region whose two verdicts differ prints as `RESCUED`, with the reading and
//! its `cjk_share` beside it, so the change is arguable from the text rather
//! than from a count.
//!
//! By default only the interesting lines are printed: the rescues, and the
//! regions the reader was offered and declined. `--all` prints every region.
//! `--time` reads one crop per page with a stopwatch around it, which is the
//! per-region cost of the rescue on whatever provider `Automatic` picks.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use cleaner_core::accel::Preference;
use cleaner_core::balloon::{self, BalloonDetector, Detected};
use cleaner_core::detect::Detector;
use cleaner_core::gate::{Ocr, OutsideText, ScriptGate, Verdict, cjk_share};
use cleaner_core::image::decode;
use cleaner_core::text_groups::{self, EvidenceModel, Execution, ModelUse, SpatialInput};

const DETECTOR: &str = "comictextdetector.onnx";
const BALLOONS: &str = "comic-text-and-bubble-detector-detector-v4-s_int8.onnx";
const GATE_MODEL: &str = "image-script-identification-osd_lstm.onnx";
const GATE_LABELS: &str = "image-script-identification-osd_labels.json";
const OCR_ENCODER: &str = "manga-ocr-encoder_model.onnx";
const OCR_DECODER: &str = "manga-ocr-decoder_model.onnx";
const OCR_VOCAB: &str = "manga-ocr-vocab.txt";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut models = PathBuf::from("models");
    let mut all = false;
    let mut time = false;
    let mut pages: Vec<PathBuf> = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--models" => models = PathBuf::from(args.next().context("--models needs a path")?),
            "--all" => all = true,
            "--time" => time = true,
            other => pages.push(PathBuf::from(other)),
        }
    }
    if pages.is_empty() {
        return Err(anyhow!(
            "usage: spike-ocr-probe [--models DIR] [--all] [--time] <page.png> ..."
        ));
    }

    cleaner_core::runtime::load(&cleaner_core::runtime::find(None).context("ONNX Runtime")?)?;
    let preference = Preference::Automatic;
    let mut detector = Detector::open(&models.join(DETECTOR), preference)?;
    let mut balloons = BalloonDetector::open(&models.join(BALLOONS), preference)?;

    // Two gates over one set of weights: the same 3.7 MB model is opened twice
    // so that the *only* difference between the two verdicts is the reader.
    // Judging once and re-judging would let the first pass' state answer the
    // second, and the OSD session is stateless but this is the claim the probe
    // exists to make.
    let mut bare = ScriptGate::open(&models.join(GATE_MODEL), &models.join(GATE_LABELS), preference)?;

    let opened = Instant::now();
    let ocr = Ocr::open(
        &models.join(OCR_ENCODER),
        &models.join(OCR_DECODER),
        &models.join(OCR_VOCAB),
        preference,
    )?;
    println!(
        "OCR opened in {:.2} s on {:?}",
        opened.elapsed().as_secs_f32(),
        ocr.selection().accelerator
    );
    let mut rescuing =
        ScriptGate::open(&models.join(GATE_MODEL), &models.join(GATE_LABELS), preference)?
            .with_ocr(ocr);

    // A second reader, held apart from the gate, so the probe can print what
    // was read on a region the gate did not change its mind about - and time
    // one read without the gate's own work in the measurement.
    let mut reader = Ocr::open(
        &models.join(OCR_ENCODER),
        &models.join(OCR_DECODER),
        &models.join(OCR_VOCAB),
        preference,
    )?;

    for path in pages {
        let bytes = std::fs::read(&path)?;
        let page = decode(&bytes).map_err(|e| anyhow!("{e}"))?;
        let mut detection = detector.detect(&page)?;
        let boxes = balloons.detect(&page)?;
        // The run's regions for these models: text groups of CTD's
        // segmentation and both detectors' boxes.
        let pixels: Vec<u8> = detection
            .segmentation
            .levels
            .iter()
            .map(|&v| if v >= cleaner_core::detect::MASK_THRESHOLD { 255 } else { 0 })
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
        let mut regions: Vec<_> = grouping
            .cleaning()
            .map(|group| grouping.region(group, median))
            .chain(grouping.detector_only().map(|group| grouping.detector_only_region(group, median)))
            .collect();
        grouping.paint_estimates(&mut detection.segmentation);
        cleaner_core::detect::sort_regions(&mut regions);

        let mut rescued = 0usize;
        let mut offered = 0usize;
        println!("PAGE {} {}x{} regions={}", path.display(), page.width, page.height, regions.len());

        for (i, region) in regions.iter().enumerate() {
            let detected = balloon::detected(region.masking, &boxes);
            let inside = balloon::in_bubble(
                &page,
                &detection.segmentation,
                region.text_bounds(),
                region.masking,
                &boxes,
            );
            let before = bare.judge(
                &page,
                &detection.segmentation,
                region,
                detected,
                inside,
                OutsideText::Review,
            )?;
            let after = rescuing.judge(
                &page,
                &detection.segmentation,
                region,
                detected,
                inside,
                OutsideText::Review,
            )?;

            let m = region.masking;
            let t = region.text_bounds();
            let box_of = format!("{},{},{}x{}", m.x, m.y, m.w, m.h);
            // What the reader would say here, whether or not the gate asked it.
            // Only for the regions the gate could actually offer it, because a
            // reader run over every region on every page is minutes of probe.
            let interesting = before != after
                || (detected == Detected::TextInBubble
                    && matches!(before, Verdict::Uncertain | Verdict::NotJapanese { .. }));
            if interesting {
                offered += 1;
                let reading = reader.read(&page, t)?;
                let share = cjk_share(&reading.text);
                if before != after {
                    rescued += 1;
                    println!(
                        "RESCUED {i} box={box_of} text={},{},{}x{} before={} after={} share={share:.2} score={:.2} read={:?}",
                        t.x, t.y, t.w, t.h, name(&before), name(&after), reading.score, reading.text
                    );
                } else {
                    println!(
                        "DECLINED {i} box={box_of} detected={detected:?} verdict={} share={share:.2} score={:.2} read={:?}",
                        name(&before), reading.score, reading.text
                    );
                }
            } else if all {
                println!(
                    "REGION {i} box={box_of} detected={detected:?} verdict={}",
                    name(&before)
                );
            }
        }
        println!("SUMMARY {} offered={offered} rescued={rescued}", path.display());

        if time {
            if let Some(region) = regions.first() {
                // Warm, then measured: the first read of a session pays for
                // whatever the provider lazily builds, and that cost is the
                // session's rather than the region's.
                let _ = reader.read(&page, region.text_bounds())?;
                let started = Instant::now();
                let reading = reader.read(&page, region.text_bounds())?;
                println!(
                    "TIMING {} one read {:.0} ms for {} chars",
                    path.display(),
                    started.elapsed().as_secs_f32() * 1000.0,
                    reading.text.chars().count()
                );
            }
        }
    }
    Ok(())
}

fn name(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Clean { script } => format!("Clean({script})"),
        Verdict::NotJapanese { script } => format!("NotJapanese({script})"),
        other => format!("{other:?}"),
    }
}
