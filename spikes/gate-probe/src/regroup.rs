//! The grouping made again from a detected chapter's evidence, with the text
//! reader asked about each unit before the reach.
//!
//! ```text
//! spike-gate-probe --regroup <copy/cN.mtclean> --out DIR [--models DIR] [--clean-latin]
//! ```
//!
//! Reads a **copy** of a library chapter, as [`crate::chapter`] does. Each
//! evidence file holds the mask and the boxes one page's grouping was made
//! from, so the grouping is made again from exactly them, twice: with no
//! reader (`plain`), and with Hayai answering for each unit (`checked`,
//! [`cleaner_core::text_groups::group_checked`]). The model tile seams of the
//! first run are not in the file, so `plain` can differ from the stored
//! grouping where a seam joined two boxes; the line says whether it does.
//!
//! One JSON line per page in `<chapter>.regroup.jsonl`: the groups of both
//! groupings by their component ids, and every question put to the reader
//! with its reading, its answer and its time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use cleaner_core::accel::Preference;
use cleaner_core::balloon::{BalloonBox, BalloonClass};
use cleaner_core::detect::{DetBox, DetectedLanguage};
use cleaner_core::engines::model::page_crop;
use cleaner_core::gate::hayai::Hayai;
use cleaner_core::gate::{self, LatinText};
use cleaner_core::image::decode;
use cleaner_core::mask::Rect;
use cleaner_core::text_groups::{self, BoxKind, EvidenceFile, Grouping, UnitAnswer};
use serde_json::{Value, json};

const HAYAI: [&str; 3] = ["hayai-ocr-vision.onnx", "hayai-ocr-decoder.onnx", "hayai-ocr-tokenizer.json"];

pub fn run(manifest_path: &Path, out: &Path, models: &Path, latin: LatinText) -> Result<()> {
    let manifest: Value = serde_json::from_slice(&std::fs::read(manifest_path)?).context("manifest")?;
    let project = manifest_path.parent().context("manifest has no folder")?;
    let chapter = manifest_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let sidecar = PathBuf::from(format!("{}.d", manifest_path.display()));

    let mut pages: BTreeMap<String, u64> = BTreeMap::new();
    for row in manifest["detections"].as_array().into_iter().flatten() {
        if let (Some(reference), Some(page)) = (row["group"]["evidence_ref"].as_str(), row["source_idx"].as_u64()) {
            pages.entry(reference.to_owned()).or_insert(page);
        }
    }
    for row in manifest["patches"].as_array().into_iter().flatten() {
        let group = &row["provenance"]["params_snapshot"]["group"];
        if let (Some(reference), Some(page)) = (group["evidence"].as_str(), row["source_idx"].as_u64()) {
            pages.entry(reference.to_owned()).or_insert(page);
        }
    }
    let mut by_page: Vec<(u64, String)> = pages.into_iter().map(|(reference, page)| (page, reference)).collect();
    by_page.sort();

    let mut reader = Hayai::open(&models.join(HAYAI[0]), &models.join(HAYAI[1]), &models.join(HAYAI[2]), Preference::Automatic)
        .map_err(|e| anyhow!("{e}"))?;
    std::fs::create_dir_all(out)?;
    let mut lines = String::new();
    for (source_idx, reference) in by_page {
        let source = &manifest["sources"][source_idx as usize];
        let rel = source["rel_path"].as_str().context("source without a path")?;
        let page = decode(&std::fs::read(project.join(rel)).with_context(|| rel.to_owned())?).map_err(|e| anyhow!("{e}"))?;
        let evidence = EvidenceFile::decode(&std::fs::read(sidecar.join(&reference)).with_context(|| reference.clone())?)
            .map_err(|e| anyhow!("{reference}: {e}"))?;
        let stored = &evidence.grouping;
        let (ox, oy) = stored.origin;
        if ox < 0 || oy < 0 || ox + i64::from(stored.width) > i64::from(page.width)
            || oy + i64::from(stored.height) > i64::from(page.height)
        {
            continue;
        }
        let whole = ox == 0 && oy == 0 && stored.width == page.width && stored.height == page.height;
        let crop = if whole { page } else { page_crop(&page, Rect::new(ox, oy, stored.width, stored.height)) };

        // The boxes in their input order: the ids name input positions.
        let (mut rt, mut ctd) = (Vec::new(), Vec::new());
        for item in &stored.boxes {
            let class = match item.kind {
                BoxKind::Bubble => BalloonClass::Bubble,
                BoxKind::TextBubble => BalloonClass::TextInBubble,
                BoxKind::TextFree => BalloonClass::TextFree,
                BoxKind::CtdText => {
                    ctd.push(DetBox { rect: item.rect, confidence: item.score, language: DetectedLanguage::Japanese });
                    continue;
                }
            };
            rt.push(BalloonBox { rect: item.rect, class, score: item.score });
        }
        let mut inputs = text_groups::Inputs::new(stored.width, stored.height, Some(&evidence.mask));
        inputs.pixel_model = stored.pixel_model;
        inputs.page = Some(&crop);
        inputs.origin = stored.origin;
        inputs.rt = &rt;
        inputs.ctd = &ctd;
        inputs.models = stored.models.clone();

        let plain = text_groups::group(&inputs).map_err(|e| anyhow!("{e}"))?;
        let mut asked: Vec<Value> = Vec::new();
        let mut check = |rect: Rect| -> UnitAnswer {
            let started = Instant::now();
            // As the run reads a unit: by the rectangle a region of it has.
            let rect = cleaner_core::detect::tight_rect(rect, crop.width, crop.height);
            let reading = reader.read(&crop, rect).ok();
            let answer = gate::unit_answer(reading.as_ref(), latin);
            asked.push(json!({
                "rect": [rect.x + ox, rect.y + oy, rect.w, rect.h],
                "text": reading.as_ref().map(|r| r.text.clone()),
                "confidence": reading.as_ref().map(|r| r.confidence),
                "floor": reading.as_ref().map(|r| r.floor),
                "answer": format!("{answer:?}"),
                "ms": started.elapsed().as_secs_f64() * 1000.0,
            }));
            answer
        };
        let checked = text_groups::group_checked(&inputs, Some(&mut check)).map_err(|e| anyhow!("{e}"))?;
        let line = json!({
            "chapter": chapter,
            "sourceIdx": source_idx,
            "page": rel,
            "evidence": reference,
            "plainMatchesStored": same_groups(&plain, stored),
            "stored": groups_of(stored),
            "plain": groups_of(&plain),
            "checked": groups_of(&checked),
            "asked": asked,
        });
        lines.push_str(&line.to_string());
        lines.push('\n');
        eprintln!("{chapter} page {source_idx}: {} groups, {} with the reader", plain.groups.len(), checked.groups.len());
    }
    std::fs::write(out.join(format!("{chapter}.regroup.jsonl")), lines)?;
    Ok(())
}

fn groups_of(grouping: &Grouping) -> Vec<Value> {
    let (ox, oy) = grouping.origin;
    grouping.groups.iter().map(|group| json!({
        "id": group.id,
        "origin": group.origin,
        "disposition": group.disposition,
        "reasons": group.reasons,
        "bounds": [group.bounds.x + ox, group.bounds.y + oy, group.bounds.w, group.bounds.h],
        "boxes": group.box_ids,
        "components": group.component_ids,
        "pixels": group.lettering_pixels,
        "estimated": group.estimated,
    })).collect()
}

/// Whether two groupings put the same components in the same groups.
fn same_groups(a: &Grouping, b: &Grouping) -> bool {
    let sets = |grouping: &Grouping| {
        let mut sets: Vec<(Vec<String>, Vec<String>)> = grouping.groups.iter()
            .map(|group| (group.component_ids.clone(), group.box_ids.clone()))
            .collect();
        sets.sort();
        sets
    };
    sets(a) == sets(b)
}
