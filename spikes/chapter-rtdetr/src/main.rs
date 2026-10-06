//! Run the application's RT-DETR-v2 balloon/text detector once per page while
//! retaining one model session. This is a measurement probe, not app routing.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use cleaner_core::accel::Preference;
use cleaner_core::balloon::{BalloonClass, BalloonDetector};
use cleaner_core::image::foreign::decode;
use serde::Serialize;

#[derive(Serialize)]
struct BoxRow {
    class: &'static str,
    score: f32,
    x: i64,
    y: i64,
    width: u32,
    height: u32,
}

#[derive(Serialize)]
struct PageRow {
    file: String,
    width: u32,
    height: u32,
    decode_ms: f64,
    detect_ms: f64,
    boxes: Vec<BoxRow>,
}

#[derive(Serialize)]
struct Report {
    model: String,
    runtime: String,
    provider: String,
    model_open_ms: f64,
    source_dir: String,
    pages: Vec<PageRow>,
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(args.len() == 3, "usage: chapter-rtdetr <source-dir> <model.onnx> <output.json>");
    let source = PathBuf::from(&args[0]);
    let model = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);

    let runtime = cleaner_core::runtime::find(None)?;
    cleaner_core::runtime::load(&runtime)?;
    let start = Instant::now();
    let mut detector = BalloonDetector::open(&model, Preference::Automatic)?;
    let model_open_ms = start.elapsed().as_secs_f64() * 1000.0;
    let provider = format!("{:?}", detector.selection());

    let mut paths: Vec<_> = fs::read_dir(&source)?
        .map(|entry| entry.map(|value| value.path()))
        .collect::<std::io::Result<_>>()?;
    paths.retain(|path| matches!(path.extension().and_then(|ext| ext.to_str()), Some("jpg" | "jpeg" | "png")));
    paths.sort();
    anyhow::ensure!(!paths.is_empty(), "no images in {}", source.display());
    let mut pages = Vec::with_capacity(paths.len());
    for path in paths {
        let started = Instant::now();
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let page = decode(&bytes).with_context(|| format!("decode {}", path.display()))?;
        let decode_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let boxes = detector.detect(&page).with_context(|| format!("detect {}", path.display()))?;
        let detect_ms = started.elapsed().as_secs_f64() * 1000.0;
        let boxes = boxes.into_iter().map(|row| BoxRow {
            class: match row.class {
                BalloonClass::Bubble => "bubble",
                BalloonClass::TextInBubble => "text_bubble",
                BalloonClass::TextFree => "text_free",
            },
            score: row.score,
            x: row.rect.x,
            y: row.rect.y,
            width: row.rect.w,
            height: row.rect.h,
        }).collect::<Vec<_>>();
        println!("{}: {} boxes, {:.1} ms", path.display(), boxes.len(), detect_ms);
        pages.push(PageRow {
            file: path.file_name().unwrap().to_string_lossy().into_owned(),
            width: page.width,
            height: page.height,
            decode_ms,
            detect_ms,
            boxes,
        });
    }
    let report = Report {
        model: absolute(&model)?.display().to_string(),
        runtime: absolute(&runtime)?.display().to_string(),
        provider,
        model_open_ms,
        source_dir: absolute(&source)?.display().to_string(),
        pages,
    };
    if let Some(parent) = output.parent() { fs::create_dir_all(parent)?; }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("wrote {}", output.display());
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    path.canonicalize().with_context(|| format!("resolve {}", path.display()))
}
