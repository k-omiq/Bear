//! Offline comparison of saved FLUX inputs with the production LaMa renderer.
//! Reads copied evidence and cached model files; writes only a fresh quality folder.

use std::fs;
use std::path::Path;

use cleaner_core::accel::Preference;
use cleaner_core::composite;
use cleaner_core::engines::{lama, model};
use cleaner_core::fit;
use cleaner_core::image::{self, BitDepth, ColorMode, Format, Raster};
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::project::Job;
use serde_json::{Value, json};

const EVIDENCE: &str = "/Users/caved/dev/manga-release-validation-2026-09-27/evidence";
const STAGES: &str = "/private/tmp/claude-501/-Users-caved-dev-manga-cleaner/91c8cbd2-8f58-4d81-a78e-9448a6df5765/scratchpad/mask/stages";
const OUTPUT: &str = "/Users/caved/dev/manga-release-validation-2026-09-27/quality/lama";
const MODEL: &str = "/Users/caved/Library/Application Support/com.mangacleaner.studio/models/lama-manga.onnx";
const RUNTIME: &str = "/Users/caved/dev/manga-cleaner/runtimes/onnxruntime-osx-arm64-1.28.0/lib/libonnxruntime.dylib";
const REGIONS: [&str; 6] = [
    "c32-p004-d107", "c32-p004-d81", "c32-p006-d190",
    "c32-p002-d27", "c32-p006-d212", "c32-p004-d67",
];

fn read_json(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn stage_rect(meta: &Value) -> Result<Rect, Box<dyn std::error::Error>> {
    let c = &meta["cropPage"];
    Ok(Rect::new(
        c["x"].as_i64().ok_or("missing crop x")?,
        c["y"].as_i64().ok_or("missing crop y")?,
        c["w"].as_u64().ok_or("missing crop width")? as u32,
        c["h"].as_u64().ok_or("missing crop height")? as u32,
    ))
}

fn rgb_crop(page: &Raster, rect: Rect) -> Raster {
    let page = image::proxy::display(page).expect("supported managed preview");
    let mut data = Vec::with_capacity(rect.w as usize * rect.h as usize * 3);
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let px = x.clamp(0, page.width as i64 - 1) as u32;
            let py = y.clamp(0, page.height as i64 - 1) as u32;
            for c in 0..3 {
                data.push(page.sample(px, py, model::source_channel(page.mode, c)) as u8);
            }
        }
    }
    Raster {
        width: rect.w, height: rect.h, mode: ColorMode::Rgb, depth: BitDepth::Eight,
        icc: None, palette: None, trns: None, srgb_intent: Some(1), color: Default::default(), data,
    }
}

fn mask_crop(mask: &Mask, rect: Rect) -> Raster {
    let mut data = Vec::with_capacity(rect.w as usize * rect.h as usize);
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            data.push(if mask.contains(x, y) { 255 } else { 0 });
        }
    }
    Raster {
        width: rect.w, height: rect.h, mode: ColorMode::Gray, depth: BitDepth::Eight,
        icc: None, palette: None, trns: None, srgb_intent: Some(1), color: Default::default(), data,
    }
}

fn save_png(path: &Path, raster: &Raster) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(path, image::encode(raster, Format::Png)?)?;
    Ok(())
}

fn apply_render(page: &Raster, rendered: &model::Rendered) -> (Raster, u64, u64, u64) {
    let mut out = page.clone();
    let mut changed_outside = 0;
    let mut changed_alpha = 0;
    let mut returned_outside = 0;
    for y in 0..page.height {
        for x in 0..page.width {
            let inside = rendered.mask.contains(x as i64, y as i64);
            for channel in 0..page.mode.samples() {
                let before = page.sample(x, y, channel);
                let local_x = x as i64 - rendered.mask.bounds.x;
                let local_y = y as i64 - rendered.mask.bounds.y;
                if inside {
                    let after = rendered.pixels.sample(local_x as u32, local_y as u32, channel);
                    out.set_sample(x, y, channel, after);
                } else if local_x >= 0 && local_y >= 0
                    && local_x < rendered.mask.bounds.w as i64 && local_y < rendered.mask.bounds.h as i64
                    && rendered.pixels.sample(local_x as u32, local_y as u32, channel) != before
                {
                    returned_outside += 1;
                }
                if !inside && out.sample(x, y, channel) != before {
                    changed_outside += 1;
                }
                if page.mode.alpha_channel() == Some(channel) && out.sample(x, y, channel) != before {
                    changed_alpha += 1;
                }
            }
        }
    }
    (out, changed_outside, changed_alpha, returned_outside)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let evidence = Path::new(EVIDENCE);
    let stages = Path::new(STAGES);
    let output = Path::new(OUTPUT);
    fs::create_dir(output)?;
    cleaner_core::runtime::load(Path::new(RUNTIME))?;
    let mut inpainter = lama::Inpainter::open(Path::new(MODEL), Preference::CpuOnly)?;
    let current = Job::open_read_only(&evidence.join("c32.mtclean"))?;
    let snapshot = Job::open_read_only(&evidence.join("detect-snapshot/c32.mtclean"))?;
    let mut summary = Vec::new();

    for region in REGIONS {
        let saved = stages.join(region);
        let meta = read_json(&saved.join("meta.json"))?;
        let rect = stage_rect(&meta)?;
        let basis = meta["basis"].as_str().ok_or("missing stage basis")?;
        let record = snapshot.load_detection(region)?.ok_or("missing saved detection")?;
        let source_idx = record.record.source_idx;
        let source = current.source_path(source_idx).ok_or("missing copied source path")?;
        let raw = image::decode(&fs::read(source)?)?;
        let input = if basis == "current" {
            let mut patches = Vec::new();
            for prior in &current.project.patches {
                if prior.source_idx == source_idx && prior.visible && prior.order < record.record.order {
                    patches.push(current.load_patch(prior)?);
                }
            }
            composite::composite(&raw, &patches)?
        } else {
            raw.clone()
        };
        let actual_input = rgb_crop(&input, rect);
        let saved_input = image::decode(&fs::read(saved.join("input.png"))?)?;
        let input_match = actual_input.data == saved_input.data;

        // The fitted statistics are irrelevant to LaMa's renderer. Replace its
        // masks with the saved detection masks so the model hole is exact.
        let edges = fit::EdgeMap::sobel(&input);
        let mut fitted = fit::fit(&input, &record.ink, 1.0, fit::page_noise_sigma(&input), &edges, false);
        fitted.mask = record.mask.clone();
        fitted.ink = record.ink.clone();
        let rendered = inpainter.render(&input, &fitted)?;
        let (lama_page, changed_outside, changed_alpha, returned_outside) = apply_render(&input, &rendered);
        let mut hole_outside = 0;
        for y in record.ink.bounds.y..record.ink.bounds.bottom() {
            for x in record.ink.bounds.x..record.ink.bounds.right() {
                if record.ink.contains(x, y) && !rendered.mask.contains(x, y) {
                    hole_outside += 1;
                }
            }
        }
        if changed_outside != 0 || changed_alpha != 0 || returned_outside != 0 || hole_outside != 0 {
            return Err(format!("{region}: support or alpha invariant failed").into());
        }

        let dir = output.join(region);
        fs::create_dir(&dir)?;
        save_png(&dir.join("input.png"), &actual_input)?;
        save_png(&dir.join("lama.png"), &rgb_crop(&lama_page, rect))?;
        save_png(&dir.join("hole.png"), &mask_crop(&record.ink, rect))?;
        save_png(&dir.join("lama_support.png"), &mask_crop(&rendered.mask, rect))?;
        for name in ["raw.png", "composite.png", "alpha.png", "source.png"] {
            fs::copy(saved.join(name), dir.join(format!("flux_{name}")))?;
        }
        let row = json!({
            "region": region,
            "page": meta["page"],
            "basis": basis,
            "input_matches_saved_flux_crop": input_match,
            "crop": meta["cropPage"],
            "hole_pixels": record.ink.count(),
            "lama_support_pixels": rendered.mask.count(),
            "hole_outside_lama_support": hole_outside,
            "changed_outside_lama_support": changed_outside,
            "returned_outside_lama_support": returned_outside,
            "changed_alpha": changed_alpha,
            "lama_tiles": rendered.tiles,
            "lama_pad": rendered.pad.as_str(),
            "lama_model_context": "512x512 real source page, edge replicated only at page edge",
            "flux_context": "saved recipe 1.0.0 crop, scaled to 768 working long side",
        });
        fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(&row)?)?;
        summary.push(row);
        eprintln!("{region}: LaMa done; saved input match={input_match}");
    }
    fs::write(output.join("summary.json"), serde_json::to_vec_pretty(&summary)?)?;
    Ok(())
}
