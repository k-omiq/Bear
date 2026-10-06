//! Run detection on a page and draw what came back.
//!
//! A box list proves the head decoded; it does not prove the **mask** is
//! registered to the page. The letterbox is padded on two sides and the mask is
//! resized twice, so an off-by-a-scale-factor there produces a mask that looks
//! perfectly reasonable in isolation and sits 30 px up and left of the text.
//! The only way to see that is to draw it.
//!
//! ```text
//! spike-detect-page <page.png> [--out overlay.png]
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use cleaner_core::accel::Preference;
use cleaner_core::detect::{Detector, Segmentation};
use cleaner_core::image::{ColorMode, Format, Raster, decode, encode};
use cleaner_core::mask::Rect;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let page_path = PathBuf::from(
        args.next().ok_or_else(|| anyhow!("usage: spike-detect-page <page.png> [--out overlay.png]"))?,
    );
    let mut out_path = page_path.with_extension("overlay.png");
    if let Some(flag) = args.next() {
        if flag == "--out" {
            out_path = PathBuf::from(args.next().ok_or_else(|| anyhow!("--out needs a path"))?);
        }
    }

    let dylib = cleaner_core::runtime::find(None)
        .context("locating ONNX Runtime; set ORT_DYLIB_PATH or run scripts/fetch-runtime.sh")?;
    cleaner_core::runtime::load(&dylib)?;

    let model = std::path::Path::new("models/comictextdetector.onnx");
    if !model.exists() {
        return Err(anyhow!("no detector at {} - run scripts/fetch-models.sh", model.display()));
    }

    let bytes = std::fs::read(&page_path).with_context(|| format!("reading {}", page_path.display()))?;
    let page = decode(&bytes).map_err(|e| anyhow!("{e}"))?;
    println!("page: {}×{} {:?} {:?}", page.width, page.height, page.mode, page.depth);

    let started = std::time::Instant::now();
    let mut detector = Detector::open(model, Preference::Automatic)?;
    let opened = started.elapsed();
    let started = std::time::Instant::now();
    let detection = detector.detect(&page)?;
    println!(
        "session {:.2} s, detect {:.2} s",
        opened.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );

    println!("\nraw boxes: {}", detection.boxes.len());
    for b in &detection.boxes {
        println!(
            "  {:?} conf {:.2} lang {:?}",
            (b.rect.x, b.rect.y, b.rect.w, b.rect.h),
            b.confidence,
            b.language
        );
    }

    let text_pixels = detection.segmentation.levels.iter().filter(|v| **v >= 76).count();
    println!(
        "\nsegmentation: {}×{}, {} px above threshold ({:.2}% of the page)",
        detection.segmentation.width,
        detection.segmentation.height,
        text_pixels,
        100.0 * text_pixels as f64 / detection.segmentation.levels.len() as f64
    );

    let regions = cleaner_core::detect::build_regions(detection.boxes.clone(), page.width, page.height);

    // The balloon question, answered by the model whose class names those
    // documents were already using.
    let balloon_model = std::path::Path::new("models/comic-text-and-bubble-detector-detector-v4-s_int8.onnx");
    let balloons = if balloon_model.exists() {
        let started = std::time::Instant::now();
        let found = cleaner_core::balloon::BalloonDetector::open(balloon_model, Preference::Automatic)?.detect(&page)?;
        println!("\nballoons: {} in {:.2} s", found.len(), started.elapsed().as_secs_f64());
        for b in &found {
            println!(
                "  {:?} {:?} {:.2}",
                (b.rect.x, b.rect.y, b.rect.w, b.rect.h),
                b.class,
                b.score
            );
        }
        found
    } else {
        println!("\nno balloon detector - every region will read as out of balloon");
        Vec::new()
    };

    let mut gate = cleaner_core::gate::ScriptGate::open(
        std::path::Path::new("models/image-script-identification-osd_lstm.onnx"),
        std::path::Path::new("models/image-script-identification-osd_labels.json"),
        Preference::Automatic,
    )
    .ok();

    println!("\nregions: {}", regions.len());
    for r in &regions {
        let detected = cleaner_core::balloon::detected(r.masking, &balloons);
        let inside = cleaner_core::balloon::in_bubble(
            &page,
            &detection.segmentation,
            r.text_bounds(),
            r.masking,
            &balloons,
        );
        let verdict = match gate.as_mut() {
            Some(g) => Some(g.judge(
                &page,
                &detection.segmentation,
                r,
                detected,
                inside,
                cleaner_core::gate::OutsideText::Review,
            )?),
            None => None,
        };
        let orientation = cleaner_core::gate::lines::orientation(&detection.segmentation, r.masking);
        let split = cleaner_core::gate::lines::split(&detection.segmentation, r.masking, orientation);
        let tightened: Vec<(i64, i64, u32, u32)> = split
            .iter()
            .map(|l| {
                let t = cleaner_core::gate::lines::tighten(&detection.segmentation, l, orientation);
                (t.x, t.y, t.w, t.h)
            })
            .collect();
        println!(
            "  masking {:?} members {} large {} lang {:?} balloon {} {:?} lines {:?} → {:?}",
            (r.masking.x, r.masking.y, r.masking.w, r.masking.h),
            r.members.len(),
            r.flagged_large,
            r.language(),
            inside,
            orientation,
            tightened,
            verdict
        );
    }

    let overlay = draw_overlay(&page, &detection.segmentation, &regions);
    std::fs::write(&out_path, encode(&overlay, Format::Png).map_err(|e| anyhow!("{e}"))?)?;
    println!("\noverlay: {}", out_path.display());
    Ok(())
}

/// The page in gray, the mask in red, the masking boxes outlined in green.
fn draw_overlay(
    page: &Raster,
    seg: &Segmentation,
    regions: &[cleaner_core::detect::Region],
) -> Raster {
    let mut out = Raster {
        width: page.width,
        height: page.height,
        mode: ColorMode::Rgb,
        depth: cleaner_core::image::BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data: vec![0; (page.width as usize) * (page.height as usize) * 3],
    };

    for y in 0..page.height {
        for x in 0..page.width {
            let [r, g, b] = page.rgb8_pixel(x, y);
            let level = seg.at(x, y);
            // Red where the mask is hot, proportionally, so a soft edge is
            // visible as a soft edge rather than as a hard boundary the
            // threshold invented.
            let t = level as u32;
            let mix = |c: u8, target: u32| ((c as u32 * (255 - t) + target * t) / 255) as u16;
            out.set_sample(x, y, 0, mix(r, 255));
            out.set_sample(x, y, 1, mix(g, 0));
            out.set_sample(x, y, 2, mix(b, 0));
        }
    }

    for region in regions {
        outline(&mut out, region.masking, [0, 200, 0]);
        for member in &region.members {
            outline(&mut out, member.tight, [0, 90, 255]);
        }
    }
    out
}

fn outline(raster: &mut Raster, rect: Rect, colour: [u16; 3]) {
    let mut put = |x: i64, y: i64| {
        if x >= 0 && y >= 0 && x < raster.width as i64 && y < raster.height as i64 {
            for (channel, value) in colour.iter().enumerate() {
                raster.set_sample(x as u32, y as u32, channel, *value);
            }
        }
    };
    for t in 0..3 {
        for x in rect.x..rect.right() {
            put(x, rect.y + t);
            put(x, rect.bottom() - 1 - t);
        }
        for y in rect.y..rect.bottom() {
            put(rect.x + t, y);
            put(rect.right() - 1 - t, y);
        }
    }
}
