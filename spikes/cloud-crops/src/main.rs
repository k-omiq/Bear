//! Cloud render inputs from real pages, and model answers composited back.
//!
//! ```text
//! spike-cloud-crops export    <out-dir> <page.png>...
//! spike-cloud-crops composite <out-dir> <results-dir> <page.png>...
//! ```
//!
//! `export` detects each page the way a Detect run does and, for every region
//! the gate would clean, writes what a cloud render of it is sent: the
//! preprocessing V2 crop, the hint, and the weight the answer is written at.
//! Under `<out-dir>/<page>_<nn>/`: `crop.png`, `hint.png`, `alpha.png`,
//! `meta.json`.
//!
//! `composite` detects again (detection is deterministic, and `meta.json`'s
//! crop is checked against it), then for every `<results-dir>/<variant>/<region>.png`
//! composites that answer through [`PreparedRender::composite`], tone
//! alignment included, into a copy of the page. It writes `<region>.composite.png`
//! (the crop as the page now shows it), `pages/<page>.png`, and `scores.jsonl`:
//! the text detector run over the composited page, and how much text it still
//! finds on each region's lettering. `original.scores.jsonl` is the same for
//! the untouched page.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use cleaner_core::accel::Preference;
use cleaner_core::detect::{Detector, build_regions_separated};
use cleaner_core::engines::render::{GeneratedCrop, PreparedRender, Preprocessing};
use cleaner_core::fit::{self, EdgeMap};
use cleaner_core::image::{BitDepth, ColorMode, Format, Raster, decode, encode};
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::strip::{self, Strip};
use manga_cleaner_lib::run::{
    Cleaner, HIGHEST_LOCAL, PageContext, Pipeline, RegionOutcome, model_search_paths,
};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: spike-cloud-crops export <out-dir> <page>... | composite <out-dir> <results-dir> <page>...";
    let (mode, rest) = args.split_first().ok_or_else(|| anyhow!(usage))?;
    let (out_dir, results, pages) = match mode.as_str() {
        "export" if rest.len() >= 2 => (PathBuf::from(&rest[0]), None, &rest[1..]),
        "composite" if rest.len() >= 3 => (PathBuf::from(&rest[0]), Some(PathBuf::from(&rest[1])), &rest[2..]),
        _ => bail!(usage),
    };
    std::fs::create_dir_all(&out_dir)?;

    cleaner_core::runtime::load(&cleaner_core::runtime::find(None).context("ONNX Runtime")?)?;
    let models = model_search_paths(None)
        .into_iter()
        .chain(std::iter::once(PathBuf::from("../models")))
        .find(|dir| dir.join("comictextdetector.onnx").exists())
        .context("no model weights - run scripts/fetch-models.sh")?;
    let mut pipeline = Pipeline::open(&models, Preference::Automatic).map_err(|e| anyhow!("{e}"))?;

    let detector_model = models.join("comictextdetector.onnx");
    let mut detector = Detector::open(&detector_model, Preference::Automatic).map_err(|e| anyhow!("{e}"))?;
    let mut total = 0usize;
    for page_path in pages {
        let page_path = Path::new(page_path);
        let stem = page_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let bytes = std::fs::read(page_path)?;
        let page = decode(&bytes).map_err(|e| anyhow!("{e}"))?;
        let prepared = prepare_page(&mut pipeline, &mut detector, &stem, &bytes, &page)?;
        let ids: Vec<String> = (0..prepared.len()).map(|i| format!("{stem}_{i:02}")).collect();
        match &results {
            None => {
                for ((prepared, route), id) in prepared.iter().zip(&ids) {
                    export(&out_dir.join(id), prepared, route, &stem, &page)?;
                }
            }
            Some(results) => {
                let original = rgb8(&page)?;
                score(&mut detector, &page, &original, &prepared, &ids, &results.join("original.scores.jsonl"))?;
                for variant in std::fs::read_dir(results)? {
                    let variant = variant?.path();
                    if variant.is_dir() {
                        total += composite_page(&out_dir, &variant, &stem, &ids, &prepared, &page, &original, &mut detector)?;
                    }
                }
            }
        }
        println!("{stem}: {} regions", prepared.len());
    }
    if results.is_some() {
        println!("{total} answers composited");
    }
    Ok(())
}

/// Every region the gate would clean on `page`, prepared as a cloud render of
/// it is: the detection's mask taken as given, its lettering and fit put
/// back, as `region.rs` does for a click on a stored detection.
fn prepare_page(pipeline: &mut Pipeline, detector: &mut Detector, stem: &str, bytes: &[u8], page: &Raster) -> Result<Vec<(PreparedRender, String)>> {
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let survey = strip::survey(&strip, |_| None);
    let context = PageContext { strip: &strip, joins: &survey.joins, segments: &survey.segments, placement: 0, sources: &[] };
    let outcome = pipeline.detect_page(stem, bytes, HIGHEST_LOCAL, &context).map_err(|e| anyhow!("{e}"))?;
    let noise = fit::page_noise_sigma(page);
    let edges = EdgeMap::sobel(page);
    let mut out = Vec::new();
    let mut held = Vec::new();
    for region in &outcome.regions {
        let found = match region {
            RegionOutcome::Detected(found) => found,
            RegionOutcome::Candidate { bbox, .. } | RegionOutcome::Untouched { bbox, .. } => {
                held.push(*bbox);
                continue;
            }
            // Only a clean pass declines; Detect never does.
            RegionOutcome::Cleaned(..) | RegionOutcome::Declined { .. } => continue,
        };
        let mut fitted = fit::fit(page, &found.mask, 1.0, noise, &edges, true);
        fitted.ink = found.ink.clone();
        if let Some(stored) = found.record.fit {
            fitted.route = stored.route;
            fitted.thickness = stored.thickness;
            fitted.best_deviation = stored.best_deviation;
        }
        let route = format!("{:?}", fitted.route);
        match PreparedRender::prepare_as(page, &fitted, Preprocessing::V2) {
            Ok(prepared) => out.push((prepared, route)),
            Err(e) => println!("  {stem}: region at {:?} not renderable: {e}", found.record.bbox),
        }
    }
    // What the gate held back, cleaned as a click on it is (`region.rs`
    // `Bench::text_under`): the detector's region that overlaps the box most,
    // its segmentation as the seed, and the fit's own search.
    if !held.is_empty() {
        let detection = detector.detect(page).map_err(|e| anyhow!("{e}"))?;
        let regions = build_regions_separated(detection.boxes.clone(), page.width, page.height, |a, b| {
            cleaner_core::balloon::merge_crosses_a_balloon(page, &detection.segmentation, a, b)
        });
        for bbox in held {
            let Some(region) = regions.iter().max_by_key(|r| overlap(r.masking, bbox)).filter(|r| overlap(r.masking, bbox) > 0) else {
                println!("  {stem}: held-back box {bbox:?} has no detected text under it");
                continue;
            };
            let seed = fit::seed_mask(&detection.segmentation, region, page.width, page.height);
            if seed.is_empty() {
                continue;
            }
            let fitted = fit::fit(page, &seed, detection.segmentation.proxy_scale(), noise, &edges, false);
            let route = format!("held:{:?}", fitted.route);
            match PreparedRender::prepare_as(page, &fitted, Preprocessing::V2) {
                Ok(prepared) => out.push((prepared, route)),
                Err(e) => println!("  {stem}: held-back box {bbox:?} not renderable: {e}"),
            }
        }
    }
    // Hand masks, for lettering the detector does not see (a sound effect
    // drawn as art): `$MASKS_DIR/<page>__<name>.png`, page sized, nonzero
    // where the user brushed. Taken as a brushed mask is (`manual`), or with
    // `MASKS_AS=detect` as a Detect run takes SAM-TS lettering outside a
    // balloon (`run.rs`): the effect's outline added, then the detecting fit
    // and its growth, at the segmentation's proxy scale.
    if let Ok(dir) = std::env::var("MASKS_DIR") {
        let detect_scale = match std::env::var("MASKS_AS").as_deref() {
            Ok("detect") => Some(detector.detect(page).map_err(|e| anyhow!("{e}"))?.segmentation.proxy_scale()),
            Ok("brush") | Err(_) => None,
            Ok(other) => bail!("MASKS_AS={other}: expected brush or detect"),
        };
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&format!("{stem}__")) && n.ends_with(".png")))
            .collect();
        files.sort();
        for file in files {
            let raster = decode(&std::fs::read(&file)?).map_err(|e| anyhow!("{e}"))?;
            if (raster.width, raster.height) != (page.width, page.height) {
                bail!("{}: not page sized", file.display());
            }
            let on: Vec<(u32, u32)> = (0..page.height)
                .flat_map(|y| (0..page.width).map(move |x| (x, y)))
                .filter(|&(x, y)| raster.sample(x, y, 0) > 0)
                .collect();
            let (Some(x0), Some(y0)) = (on.iter().map(|p| p.0).min(), on.iter().map(|p| p.1).min()) else { continue };
            let (x1, y1) = (on.iter().map(|p| p.0).max().unwrap(), on.iter().map(|p| p.1).max().unwrap());
            // Bounded by the brushed pixels, as a brushed mask is.
            let mut seed = Mask::empty(Rect::new(x0 as i64, y0 as i64, x1 - x0 + 1, y1 - y0 + 1));
            for (x, y) in on {
                seed.set(x as i64, y as i64, true);
            }
            let (fitted, kind) = match detect_scale {
                Some(scale) => {
                    let seed = fit::outlined(&seed, scale, Rect::new(0, 0, page.width, page.height));
                    (fit::fit(page, &seed, scale, noise, &edges, false), "sam")
                }
                None => (fit::fit(page, &seed, 1.0, noise, &edges, true), "hand"),
            };
            let route = format!("{kind}:{:?}", fitted.route);
            match PreparedRender::prepare_as(page, &fitted, Preprocessing::V2) {
                Ok(prepared) => out.push((prepared, route)),
                Err(e) => println!("  {stem}: hand mask {} not renderable: {e}", file.display()),
            }
        }
    }
    Ok(out)
}

fn overlap(a: Rect, b: Rect) -> u64 {
    let w = (a.right().min(b.right()) - a.x.max(b.x)).max(0) as u64;
    let h = (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0) as u64;
    w * h
}

fn export(dir: &Path, prepared: &PreparedRender, route: &str, stem: &str, page: &Raster) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let crop = prepared.crop();
    std::fs::write(dir.join("crop.png"), png(crop.w, crop.h, ColorMode::Rgb, prepared.image_rgb8().to_vec())?)?;
    std::fs::write(dir.join("hint.png"), png(crop.w, crop.h, ColorMode::Gray, prepared.hint_gray8().to_vec())?)?;
    let mut alpha = Vec::with_capacity((crop.w * crop.h) as usize);
    for y in 0..crop.h as i64 {
        for x in 0..crop.w as i64 {
            alpha.push((prepared.alpha(crop.x + x, crop.y + y) * 255.0).round() as u8);
        }
    }
    std::fs::write(dir.join("alpha.png"), png(crop.w, crop.h, ColorMode::Gray, alpha)?)?;
    let meta = serde_json::json!({
        "page": stem,
        "crop": rect_json(crop),
        "bounds": rect_json(prepared.bounds()),
        "route": route,
        "preprocessing": "2.0.0",
        "page_size": [page.width, page.height],
    });
    std::fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
    Ok(())
}

/// Composite every answer `variant` has for this page's regions into one copy
/// of the page, write each region's composited crop and the page, and score
/// what text the detector still finds there. Returns how many it composited.
#[allow(clippy::too_many_arguments)]
fn composite_page(
    out_dir: &Path,
    variant: &Path,
    stem: &str,
    ids: &[String],
    prepared: &[(PreparedRender, String)],
    page: &Raster,
    original: &[u8],
    detector: &mut Detector,
) -> Result<usize> {
    let mut shown = original.to_vec();
    let mut done = Vec::new();
    for ((prepared, _), id) in prepared.iter().zip(ids) {
        let answer = variant.join(format!("{id}.png"));
        if !answer.is_file() {
            continue;
        }
        let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(out_dir.join(id).join("meta.json"))?)?;
        let crop = prepared.crop();
        if meta["crop"] != rect_json(crop) {
            bail!("{id}: detection is not what export saw ({} vs {})", meta["crop"], rect_json(crop));
        }
        let raster = decode(&std::fs::read(&answer)?).map_err(|e| anyhow!("{e}"))?;
        if (raster.width, raster.height) != (crop.w, crop.h) {
            bail!("{}: {}x{} for a {}x{} crop", answer.display(), raster.width, raster.height, crop.w, crop.h);
        }
        let rgb = rgb8(&raster)?;
        let rendered = prepared
            .composite(&GeneratedCrop::new(crop.w, crop.h, &rgb))
            .map_err(|e| anyhow!("{id}: {e}"))?;
        let patch = rgb8(&rendered.pixels)?;
        let b = rendered.mask.bounds;
        for y in 0..b.h as i64 {
            for x in 0..b.w as i64 {
                let (px, py) = (b.x + x, b.y + y);
                if px < 0 || py < 0 || px >= page.width as i64 || py >= page.height as i64 {
                    continue;
                }
                let from = ((y as usize) * b.w as usize + x as usize) * 3;
                let to = ((py as usize) * page.width as usize + px as usize) * 3;
                shown[to..to + 3].copy_from_slice(&patch[from..from + 3]);
            }
        }
        done.push((prepared, id));
    }
    if done.is_empty() {
        return Ok(0);
    }
    for (prepared, id) in &done {
        let crop = prepared.crop();
        std::fs::write(variant.join(format!("{id}.composite.png")), png(crop.w, crop.h, ColorMode::Rgb, window(&shown, page, crop))?)?;
    }
    std::fs::create_dir_all(variant.join("pages"))?;
    std::fs::write(variant.join("pages").join(format!("{stem}.png")), png(page.width, page.height, ColorMode::Rgb, shown.clone())?)?;
    let (regions, names): (Vec<(PreparedRender, String)>, Vec<String>) =
        done.iter().map(|(p, id)| (((*p).clone(), String::new()), (*id).clone())).unzip();
    score(detector, page, &shown, &regions, &names, &variant.join("scores.jsonl"))?;
    Ok(done.len())
}

/// Run the text detector over `shown` (8-bit RGB, page sized) and append, per
/// region, the mean text-segmentation level over the lettering the region was
/// asked to remove, and over its whole write mask. 0 is no text left.
fn score(detector: &mut Detector, page: &Raster, shown: &[u8], prepared: &[(PreparedRender, String)], ids: &[String], out: &Path) -> Result<()> {
    let raster = Raster { width: page.width, height: page.height, mode: ColorMode::Rgb, depth: BitDepth::Eight,
        icc: None, palette: None, trns: None, srgb_intent: None, color: Default::default(), data: shown.to_vec() };
    let found = detector.detect(&raster).map_err(|e| anyhow!("{e}"))?;
    let levels = &found.segmentation.levels;
    let mut lines = String::new();
    for ((prepared, _), id) in prepared.iter().zip(ids) {
        let crop = prepared.crop();
        let hint = prepared.hint_gray8();
        let (mut ink_sum, mut ink_n, mut all_sum, mut all_n) = (0f64, 0u64, 0f64, 0u64);
        for y in 0..crop.h as i64 {
            for x in 0..crop.w as i64 {
                let (px, py) = (crop.x + x, crop.y + y);
                if px < 0 || py < 0 || px >= page.width as i64 || py >= page.height as i64 {
                    continue;
                }
                let level = levels[(py as usize) * page.width as usize + px as usize] as f64 / 255.0;
                if hint[(y as usize) * crop.w as usize + x as usize] > 0 {
                    ink_sum += level;
                    ink_n += 1;
                }
                if prepared.alpha(px, py) > 0.0 {
                    all_sum += level;
                    all_n += 1;
                }
            }
        }
        let mean = |s: f64, n: u64| if n == 0 { serde_json::Value::Null } else { serde_json::json!(s / n as f64) };
        lines.push_str(&serde_json::json!({ "id": id, "text_on_ink": mean(ink_sum, ink_n), "text_in_mask": mean(all_sum, all_n) }).to_string());
        lines.push('\n');
    }
    use std::io::Write;
    std::fs::OpenOptions::new().create(true).append(true).open(out)?.write_all(lines.as_bytes())?;
    Ok(())
}

/// The 8-bit RGB `page`-sized buffer under `crop`, edge-replicated off the page.
fn window(full: &[u8], page: &Raster, crop: Rect) -> Vec<u8> {
    let mut out = Vec::with_capacity((crop.w * crop.h * 3) as usize);
    for y in 0..crop.h as i64 {
        for x in 0..crop.w as i64 {
            let px = (crop.x + x).clamp(0, page.width as i64 - 1) as usize;
            let py = (crop.y + y).clamp(0, page.height as i64 - 1) as usize;
            let at = (py * page.width as usize + px) * 3;
            out.extend_from_slice(&full[at..at + 3]);
        }
    }
    out
}

fn rect_json(r: Rect) -> serde_json::Value {
    serde_json::json!([r.x, r.y, r.w, r.h])
}

fn png(width: u32, height: u32, mode: ColorMode, data: Vec<u8>) -> Result<Vec<u8>> {
    let raster = Raster { width, height, mode, depth: BitDepth::Eight, icc: None, palette: None, trns: None, srgb_intent: None, color: Default::default(), data };
    encode(&raster, Format::Png).map_err(|e| anyhow!("{e}"))
}

/// A raster as 8-bit RGB, alpha dropped. Palettes are not handled: the scans are not indexed.
fn rgb8(raster: &Raster) -> Result<Vec<u8>> {
    let ceiling = ((1u32 << raster.depth.bits()) - 1) as f32;
    let at = |x, y, c| (raster.sample(x, y, c) as f32 / ceiling * 255.0).round() as u8;
    let mut out = Vec::with_capacity((raster.width * raster.height * 3) as usize);
    for y in 0..raster.height {
        for x in 0..raster.width {
            match raster.mode {
                ColorMode::Gray | ColorMode::GrayAlpha => {
                    let v = at(x, y, 0);
                    out.extend_from_slice(&[v, v, v]);
                }
                ColorMode::Rgb | ColorMode::Rgba => out.extend_from_slice(&[at(x, y, 0), at(x, y, 1), at(x, y, 2)]),
                other => bail!("{other:?} rasters are not handled"),
            }
        }
    }
    Ok(out)
}
