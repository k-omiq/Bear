//! Both text readers on given rectangles of given pages, one JSON line each.
//!
//! ```text
//! spike-gate-probe --rects <list.json> --out DIR [--models DIR]
//! ```
//!
//! `list.json` is `[{"id": "...", "page": "<image path>", "rect": [x, y, w, h]}, ...]`.
//! Each rectangle is read by Hayai and by `manga-ocr`, as the run's readers
//! read a region's text bounds, and timed. It answers "what would a reader
//! say of this piece alone" without a grouping behind the question, so a rule
//! that reads pieces before it merges them can be tried on real pages first.
//! The lines go to `DIR/rects.jsonl`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use cleaner_core::accel::Preference;
use cleaner_core::gate::Ocr;
use cleaner_core::gate::hayai::Hayai;
use cleaner_core::image::decode;
use cleaner_core::mask::Rect;
use serde_json::{Value, json};

pub fn run(list: &Path, out: &Path, models: &Path) -> Result<()> {
    let wanted: Vec<Value> = serde_json::from_slice(&std::fs::read(list)?).context("rect list")?;
    let mut by_page: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for item in &wanted {
        by_page.entry(item["page"].as_str().context("item without a page")?.to_owned()).or_default().push(item);
    }
    let mut hayai = Hayai::open(
        &models.join("hayai-ocr-vision.onnx"),
        &models.join("hayai-ocr-decoder.onnx"),
        &models.join("hayai-ocr-tokenizer.json"),
        Preference::Automatic,
    )
    .map_err(|e| anyhow!("{e}"))?;
    let mut manga = Ocr::open(
        &models.join("manga-ocr-encoder_model.onnx"),
        &models.join("manga-ocr-decoder_model.onnx"),
        &models.join("manga-ocr-vocab.txt"),
        Preference::Automatic,
    )
    .map_err(|e| anyhow!("{e}"))?;
    eprintln!("{} rectangles on {} pages, hayai on {:?}, manga-ocr on {:?}",
        wanted.len(), by_page.len(), hayai.selection().accelerator, manga.selection().accelerator);

    std::fs::create_dir_all(out)?;
    let mut lines = String::new();
    for (path, items) in by_page {
        let page = decode(&std::fs::read(&path).with_context(|| path.clone())?).map_err(|e| anyhow!("{e}"))?;
        for item in items {
            let r = item["rect"].as_array().context("item without a rect")?;
            let n = |i: usize| r.get(i).and_then(Value::as_i64).unwrap_or(0);
            let rect = Rect::new(n(0), n(1), n(2).max(0) as u32, n(3).max(0) as u32);
            let started = Instant::now();
            let first = hayai.read(&page, rect);
            let hayai_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            let second = manga.read(&page, rect);
            let manga_ms = started.elapsed().as_secs_f64() * 1000.0;
            let line = json!({
                "id": item["id"],
                "rect": item["rect"],
                "hayai": match &first {
                    Ok(r) => json!({
                        "text": r.text, "confidence": r.confidence, "floor": r.floor, "letters": r.letters(),
                        "isText": r.is_text(), "isLettering": r.is_lettering(),
                    }),
                    Err(e) => json!({ "error": e.to_string() }),
                },
                "hayaiMs": hayai_ms,
                "mangaOcr": match &second {
                    Ok(r) => json!({ "text": r.text, "score": r.score }),
                    Err(e) => json!({ "error": e.to_string() }),
                },
                "mangaOcrMs": manga_ms,
            });
            lines.push_str(&line.to_string());
            lines.push('\n');
        }
    }
    std::fs::write(out.join("rects.jsonl"), lines)?;
    Ok(())
}
