use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use cleaner_core::accel::Preference;
use cleaner_core::engines::model::page_crop;
use cleaner_core::export::{Target, export_page};
use cleaner_core::image::{BitDepth, ColorMode, Format, Raster, decode, encode};
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::patch::Patch;
use cleaner_core::strip::{self, Strip};
use manga_cleaner_lib::run::{Cleaner, HIGHEST_LOCAL, PageContext, Pipeline, RegionOutcome};
use serde::Serialize;

#[derive(Serialize)]
struct Row {
    page: String,
    region: Option<usize>,
    candidate_box: Option<Rect>,
    candidate_box_source: String,
    candidate_box_proxy: Option<Rect>,
    base_mask_bounds: Option<Rect>,
    ink_proxy_bounds: Option<Rect>,
    applied_mask_bounds: Option<Rect>,
    outcome: String,
    reason: String,
    engine: String,
    raw_crop: String,
    raw_crop_proxy: String,
    base_mask: String,
    ink_proxy: String,
    applied_mask: String,
    patch: String,
    export: String,
    missed_discovery: String,
    bad_inpainting: String,
    late_stale_edit: String,
    correction_seconds: String,
    page_ms: u128,
    page_peak_rss_bytes: u64,
}

fn required_models(dir: &Path) -> Result<()> {
    for name in [
        "comictextdetector.onnx",
        "comic-text-and-bubble-detector-detector-v4-s_int8.onnx",
        "image-script-identification-osd_lstm.onnx",
        "image-script-identification-osd_labels.json",
        "lama-manga.onnx",
    ] {
        if !dir.join(name).is_file() {
            bail!("missing model file {} in {}; install models before running the ledger", name, dir.display());
        }
    }
    Ok(())
}

fn png(path: &Path, raster: &Raster) -> Result<()> {
    std::fs::write(path, encode(raster, Format::Png).map_err(|error| anyhow!("{error}"))?)?;
    Ok(())
}

fn mask_png(path: &Path, mask: &Mask) -> Result<()> {
    let raster = Raster {
        width: mask.bounds.w,
        height: mask.bounds.h,
        mode: ColorMode::Gray,
        depth: BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data: mask.bits.clone(),
    };
    png(path, &raster)
}

fn clipped(rect: Rect, page: &Raster) -> Rect {
    let x = rect.x.max(0).min(page.width as i64);
    let y = rect.y.max(0).min(page.height as i64);
    let right = rect.right().max(x).min(page.width as i64);
    let bottom = rect.bottom().max(y).min(page.height as i64);
    Rect::new(x, y, (right - x) as u32, (bottom - y) as u32)
}

fn peak_rss_bytes() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if status != 0 { return 0; }
    let value = unsafe { usage.assume_init().ru_maxrss.max(0) as u64 };
    if cfg!(target_os = "linux") { value * 1024 } else { value }
}

fn outcome_kind(reason: &str) -> String {
    match reason {
        "review.reason.gateSkippedNotJapanese" => "script-rejected".into(),
        "review.reason.gateSkippedLowConfidence"
        | "review.reason.gateSkippedOutsideBubble"
        | "review.reason.languageSkipped"
        | "review.reason.outsideLanguageUnverified" => "held-by-policy".into(),
        "decline.reason.rungUnavailable"
        | "decline.reason.edgeEnergy"
        | "decline.reason.histogram"
        | "decline.reason.tooLarge"
        | "decline.reason.unrepresentableMode"
        | "decline.reason.unrepresentableDepth"
        | "decline.reason.depthBeyondEngine" => "engine-declined".into(),
        _ => format!("unknown:{reason}"),
    }
}

fn csv_cell(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn write_csv(path: &Path, rows: &[Row]) -> Result<()> {
    let mut csv = String::from("page,region,candidate_box,candidate_box_source,candidate_box_proxy,base_mask_bounds,ink_proxy_bounds,applied_mask_bounds,outcome,reason,engine,raw_crop,raw_crop_proxy,base_mask,ink_proxy,applied_mask,patch,export,missed_discovery,bad_inpainting,late_stale_edit,correction_seconds,page_ms,page_peak_rss_bytes\n");
    for row in rows {
        let rect_cell = |rect: Option<Rect>| rect.map(|value| serde_json::to_string(&value)).transpose();
        let fields = [
            row.page.clone(), row.region.map(|value| value.to_string()).unwrap_or_default(),
            rect_cell(row.candidate_box)?.unwrap_or_default(), row.candidate_box_source.clone(),
            rect_cell(row.candidate_box_proxy)?.unwrap_or_default(),
            rect_cell(row.base_mask_bounds)?.unwrap_or_default(),
            rect_cell(row.ink_proxy_bounds)?.unwrap_or_default(),
            rect_cell(row.applied_mask_bounds)?.unwrap_or_default(),
            row.outcome.clone(),
            row.reason.clone(), row.engine.clone(), row.raw_crop.clone(), row.raw_crop_proxy.clone(),
            row.base_mask.clone(), row.ink_proxy.clone(), row.applied_mask.clone(),
            row.patch.clone(), row.export.clone(),
            row.missed_discovery.clone(), row.bad_inpainting.clone(), row.late_stale_edit.clone(),
            row.correction_seconds.clone(), row.page_ms.to_string(), row.page_peak_rss_bytes.to_string(),
        ];
        csv.push_str(&fields.iter().map(|field| csv_cell(field)).collect::<Vec<_>>().join(","));
        csv.push('\n');
    }
    std::fs::write(path, csv)?;
    Ok(())
}

fn run_page(pipeline: &mut Pipeline, input: &Path, output: &Path, rows: &mut Vec<Row>) -> Result<()> {
    let name = input.file_stem().context("page without stem")?.to_string_lossy().into_owned();
    let source = std::fs::read(input)?;
    let page = decode(&source).map_err(|error| anyhow!("{error}"))?;
    let page_dir = output.join(&name);
    std::fs::create_dir_all(&page_dir)?;
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let survey = strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip,
        joins: &survey.joins,
        segments: &survey.segments,
        placement: 0,
        sources: &[],
    };
    let started = Instant::now();
    let outcome = pipeline.clean_page(&name, &source, HIGHEST_LOCAL, &context)
        .map_err(|error| anyhow!("{error}"))?;
    let elapsed = started.elapsed().as_millis();
    let peak = peak_rss_bytes();
    let mut patches: Vec<Patch> = Vec::new();
    let export_name = format!("{name}/export.png");
    let first_row = rows.len();
    for (index, region) in outcome.regions.into_iter().enumerate() {
        let directory = page_dir.join(format!("region-{index:04}"));
        std::fs::create_dir_all(&directory)?;
        let (candidate_box, crop_box, kind, reason, engine, patch_ref) = match &region {
            RegionOutcome::Cleaned(patch, review) => (
                None, patch.mask.bounds, "cleaned".to_owned(), review.clone().unwrap_or_default(),
                format!("{:?}", patch.provenance.engine), Some(patch.as_ref()),
            ),
            RegionOutcome::Untouched { bbox, reason } | RegionOutcome::Candidate { bbox, reason, .. } | RegionOutcome::Declined { bbox, reason, .. } => (
                Some(*bbox), *bbox, outcome_kind(reason), reason.clone(), String::new(), None,
            ),
            RegionOutcome::Detected(_) => bail!("auto clean returned a detect-only region on {name}"),
        };
        let crop_box = clipped(crop_box, &page);
        if crop_box.w == 0 || crop_box.h == 0 {
            bail!("region {index} on {name} has empty crop box");
        }
        let prefix = format!("{name}/region-{index:04}");
        let (raw_crop, raw_crop_proxy) = if patch_ref.is_some() {
            png(&directory.join("raw-crop-proxy.png"), &page_crop(&page, crop_box))?;
            (String::new(), format!("{prefix}/raw-crop-proxy.png"))
        } else {
            png(&directory.join("raw-crop.png"), &page_crop(&page, crop_box))?;
            (format!("{prefix}/raw-crop.png"), String::new())
        };
        let (ink_proxy_bounds, applied_mask_bounds, ink_proxy, applied_mask, patch_name) = match patch_ref {
            Some(patch) => {
                png(&directory.join("patch.png"), &patch.pixels)?;
                mask_png(&directory.join("ink-proxy.png"), &patch.ink)?;
                mask_png(&directory.join("applied-mask.png"), &patch.mask)?;
                patches.push(patch.clone());
                (Some(patch.ink.bounds), Some(patch.mask.bounds),
                 format!("{prefix}/ink-proxy.png"), format!("{prefix}/applied-mask.png"),
                 format!("{prefix}/patch.png"))
            }
            None => (None, None, String::new(), String::new(), String::new()),
        };
        let candidate_box_proxy = patch_ref.map(|_| crop_box);
        let box_source = if candidate_box.is_some() { "pipeline_untouched_bbox" } else { "" };
        std::fs::write(directory.join("candidate.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "box": candidate_box, "source": box_source,
            "box_proxy": candidate_box_proxy,
            "base_mask_bounds": null, "ink_proxy_bounds": ink_proxy_bounds,
            "applied_mask_bounds": applied_mask_bounds,
        }))?)?;
        rows.push(Row {
            page: name.clone(), region: Some(index), candidate_box,
            candidate_box_source: box_source.into(),
            candidate_box_proxy, base_mask_bounds: None, ink_proxy_bounds, applied_mask_bounds,
            outcome: kind, reason, engine, raw_crop, raw_crop_proxy,
            base_mask: String::new(), ink_proxy, applied_mask,
            patch: patch_name, export: export_name.clone(),
            missed_discovery: String::new(), bad_inpainting: String::new(),
            late_stale_edit: String::new(), correction_seconds: String::new(),
            page_ms: elapsed, page_peak_rss_bytes: peak,
        });
    }
    let exported = export_page(&source, &patches, Target::SameAsSource)?;
    std::fs::write(page_dir.join("export.png"), exported.bytes)?;
    let region_count = rows.len() - first_row;
    if region_count == 0 {
        rows.push(Row {
            page: name.clone(), region: None, candidate_box: None,
            candidate_box_source: String::new(), candidate_box_proxy: None,
            base_mask_bounds: None, ink_proxy_bounds: None, applied_mask_bounds: None,
            outcome: String::new(), reason: String::new(), engine: String::new(),
            raw_crop: String::new(), raw_crop_proxy: String::new(),
            base_mask: String::new(), ink_proxy: String::new(),
            applied_mask: String::new(), patch: String::new(), export: export_name,
            missed_discovery: String::new(), bad_inpainting: String::new(),
            late_stale_edit: String::new(), correction_seconds: String::new(),
            page_ms: elapsed, page_peak_rss_bytes: peak,
        });
    }
    println!("{name}: {region_count} regions, {elapsed} ms, peak RSS {peak} bytes");
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut input, mut output, mut max_pages, mut cpu_only) = (None, None, 3usize, false);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--input-dir" => input = Some(PathBuf::from(args.next().context("--input-dir needs a path")?)),
            "--output-dir" => output = Some(PathBuf::from(args.next().context("--output-dir needs a path")?)),
            "--max-pages" => max_pages = args.next().context("--max-pages needs a number")?.parse()?,
            "--cpu-only" => cpu_only = true,
            _ => bail!("unknown argument: {flag}"),
        }
    }
    let input = input.context("usage: spike-m0-ledger --input-dir DIR --output-dir DIR [--max-pages N]")?;
    let output = output.context("--output-dir required")?;
    if max_pages == 0 || max_pages > 3 { bail!("--max-pages must be 1 to 3"); }
    let models = PathBuf::from(std::env::var("MANGA_CLEANER_MODELS").unwrap_or_else(|_| {
        format!("{}/Library/Application Support/com.mangacleaner.studio/models",
                std::env::var("HOME").unwrap_or_default())
    }));
    required_models(&models)?;
    cleaner_core::runtime::load(&cleaner_core::runtime::find(models.parent()).context("ONNX Runtime missing")?)?;
    std::fs::create_dir_all(&output)?;
    let mut pages = std::fs::read_dir(&input)?
        .map(|entry| entry.map(|value| value.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    pages.retain(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("png")));
    pages.sort();
    if pages.is_empty() { bail!("no PNG pages in {}", input.display()); }
    let preference = if cpu_only { Preference::CpuOnly } else { Preference::Automatic };
    let mut pipeline = Pipeline::open(&models, preference)
        .map_err(|error| anyhow!("{error}"))?;
    let mut rows = Vec::new();
    for page in pages.iter().take(max_pages) {
        run_page(&mut pipeline, page, &output, &mut rows)?;
    }
    std::fs::write(output.join("ledger.json"), serde_json::to_vec_pretty(&rows)?)?;
    write_csv(&output.join("ledger.csv"), &rows)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::outcome_kind;

    #[test]
    fn pipeline_reasons_have_explicit_outcomes() {
        for reason in [
            "review.reason.gateSkippedLowConfidence",
            "review.reason.gateSkippedOutsideBubble",
            "review.reason.languageSkipped",
            "review.reason.outsideLanguageUnverified",
        ] {
            assert_eq!(outcome_kind(reason), "held-by-policy", "{reason}");
        }
        assert_eq!(outcome_kind("review.reason.gateSkippedNotJapanese"), "script-rejected");
        for reason in [
            "decline.reason.rungUnavailable",
            "decline.reason.edgeEnergy",
            "decline.reason.histogram",
            "decline.reason.tooLarge",
            "decline.reason.unrepresentableMode",
            "decline.reason.unrepresentableDepth",
            "decline.reason.depthBeyondEngine",
        ] {
            assert_eq!(outcome_kind(reason), "engine-declined", "{reason}");
        }
        assert_eq!(outcome_kind("review.reason.futureReason"), "unknown:review.reason.futureReason");
    }
}
