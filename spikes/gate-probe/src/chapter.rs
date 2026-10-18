//! The text question on a detected chapter, one JSON line per group.
//!
//! ```text
//! spike-gate-probe --chapter <copy/cN.mtclean> --out DIR [--models DIR] [--outside review|clean]
//! ```
//!
//! Reads a **copy** of a library chapter: its manifest, its pages and the
//! evidence files its detection rows and patches name. Each evidence file is
//! one page's grouping and the pixel mask it was made from, so the groups here
//! are the ones the run made, from the models the run used, not a second
//! detection. Nothing is written beside the manifest.
//!
//! For every group of every such page: its origin and boxes, the balloon
//! answer, the script identifier's own verdict, the verdict the run reaches
//! from them, and a Hayai reading with its time. The reader is asked about
//! every group, not only the ones `read_check` in the app sends it, so the
//! line also says which of the reader's paths the run takes and what that
//! path makes of the reading. `crops/<n>.png` is the group with paper around
//! it and `crops/<n>.pgm` its lettering over the same rectangle.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use cleaner_core::accel::Preference;
use cleaner_core::balloon::{self, BalloonBox, BalloonClass};
use cleaner_core::detect::{Letterbox, Region, Segmentation};
use cleaner_core::engines::model::page_crop;
use cleaner_core::gate::hayai::Hayai;
use cleaner_core::gate::{self, OutsideText, ScriptGate, Verdict};
use cleaner_core::image::{Format, decode, encode};
use cleaner_core::mask::Rect;
use cleaner_core::text_groups::{BoxKind, Disposition, EvidenceFile, EvidenceModel, GroupOrigin};
use serde_json::{Value, json};

const HAYAI: [&str; 3] = ["hayai-ocr-vision.onnx", "hayai-ocr-decoder.onnx", "hayai-ocr-tokenizer.json"];

/// The paper shown around a group in its crop: this share of its longer side,
/// and at least [`MIN_CONTEXT`] pixels.
const CONTEXT: f32 = 0.6;
const MIN_CONTEXT: i64 = 48;

pub fn run(manifest_path: &Path, out: &Path, models: &Path, outside: Option<OutsideText>, gate: &mut ScriptGate) -> Result<()> {
    let manifest: Value = serde_json::from_slice(&std::fs::read(manifest_path)?).context("manifest")?;
    let project = manifest_path.parent().context("manifest has no folder")?;
    let chapter = manifest_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let sidecar = PathBuf::from(format!("{}.d", manifest_path.display()));
    let settings = &manifest["settings"];
    // A solid-bubbles run holds no outside text for review (`judge_region`).
    let outside = outside.unwrap_or_else(|| {
        if settings["runSolidBubblesOnly"].as_bool() == Some(true) {
            OutsideText::Clean
        } else {
            OutsideText::from_arg(settings["runOutsideBubbles"].as_str())
        }
    });
    let all_text = settings["runTextPolicy"].as_str() == Some("all_text");

    // Evidence file -> the page it is of, from the rows that name it.
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
    eprintln!("{chapter}: {} pages with evidence, reader on {:?}, outside={outside:?}, all_text={all_text}",
        by_page.len(), reader.selection().accelerator);

    let crops = out.join("crops");
    std::fs::create_dir_all(&crops)?;
    let mut lines = String::new();
    let mut number = 0usize;
    for (source_idx, reference) in by_page {
        let source = &manifest["sources"][source_idx as usize];
        let rel = source["rel_path"].as_str().context("source without a path")?;
        let page = decode(&std::fs::read(project.join(rel)).with_context(|| rel.to_owned())?).map_err(|e| anyhow!("{e}"))?;
        let evidence = EvidenceFile::decode(&std::fs::read(sidecar.join(&reference)).with_context(|| reference.clone())?)
            .map_err(|e| anyhow!("{reference}: {e}"))?;
        let grouping = &evidence.grouping;
        let (ox, oy) = grouping.origin;
        if ox < 0 || oy < 0 || ox + i64::from(grouping.width) > i64::from(page.width)
            || oy + i64::from(grouping.height) > i64::from(page.height)
        {
            eprintln!("{chapter} page {source_idx}: evidence raster leaves the page, skipped");
            continue;
        }
        let whole = ox == 0 && oy == 0 && grouping.width == page.width && grouping.height == page.height;
        let crop = if whole { page } else { page_crop(&page, Rect::new(ox, oy, grouping.width, grouping.height)) };

        // What `detect_segment` hands the gate: the mask as the segmentation,
        // each estimate painted in, and the Ogkalu boxes as the balloons.
        let mut segmentation = Segmentation {
            width: grouping.width,
            height: grouping.height,
            levels: evidence.mask.iter().map(|&v| if v != 0 { 255 } else { 0 }).collect(),
            fit: Letterbox::fit(grouping.width, grouping.height),
        };
        grouping.paint_estimates(&mut segmentation);
        let balloons: Vec<BalloonBox> = grouping.boxes.iter().filter_map(|b| {
            let class = match b.kind {
                BoxKind::Bubble => BalloonClass::Bubble,
                BoxKind::TextBubble => BalloonClass::TextInBubble,
                BoxKind::TextFree => BalloonClass::TextFree,
                BoxKind::CtdText => return None,
            };
            Some(BalloonBox { rect: b.rect, class, score: b.score })
        }).collect();
        let median = grouping.block_median_area();

        for group in &grouping.groups {
            let region: Region = if group.disposition == Disposition::Clean && group.component_ids.is_empty() {
                grouping.detector_only_region(group, median)
            } else {
                grouping.region(group, median)
            };
            let lettering = region.group.as_ref().map(|held| held.lettering.clone());
            let text = region.text_bounds();
            let detected = balloon::detected(region.masking, &balloons);
            let interior = balloon::interior_of(&crop, &segmentation, text);
            let inside = balloon::in_bubble(&crop, &segmentation, text, region.masking, &balloons);
            // The identifier's own answer, whatever the balloon question said.
            let script = gate.judge(&crop, &segmentation, &region, detected, true, outside)?;
            let gated = if all_text { Verdict::OptedIn } else if inside { script.clone() } else {
                gate.judge(&crop, &segmentation, &region, detected, false, outside)?
            };

            let started = Instant::now();
            let reading = reader.read(&crop, text);
            let read_ms = started.elapsed().as_secs_f64() * 1000.0;

            // `read_check`, with the reading it would have had.
            let boxless = group.origin == GroupOrigin::MaskLayout;
            let seen = reading.as_ref().ok();
            let (path, after) = if gate::rescue_applies(&gated, detected) {
                ("rescue", gate::rescued_by(gated.clone(), seen))
            } else if gate::shape_rescue_applies(&gated, detected) {
                ("shapeRescue", gate::shape_rescued_by(gated.clone(), seen))
            } else if gate::overrule_applies(&gated, detected) {
                ("overrule", gate::overruled_by(gated.clone(), seen))
            } else if gated == Verdict::OptedIn || (boxless && gated.cleans()) {
                ("check", gate::text_checked(gated.clone(), seen, gate::LatinText::Clean))
            } else {
                ("notRead", gated.clone())
            };

            number += 1;
            let bounds = group.bounds;
            let grow = ((bounds.w.max(bounds.h) as f32 * CONTEXT) as i64).max(MIN_CONTEXT);
            let view = bounds.grown(grow as u32, crop.width, crop.height);
            let name = format!("{chapter}-{number:04}");
            std::fs::write(crops.join(format!("{name}.png")), encode(&page_crop(&crop, view), Format::Png).map_err(|e| anyhow!("{e}"))?)?;
            let mut pgm = format!("P5\n{} {}\n255\n", view.w, view.h).into_bytes();
            for y in view.y..view.bottom() {
                for x in view.x..view.right() {
                    pgm.push(if lettering.as_ref().is_some_and(|mask| mask.contains(x, y)) { 255 } else { 0 });
                }
            }
            std::fs::write(crops.join(format!("{name}.pgm")), pgm)?;

            let boxes: Vec<Value> = group.box_ids.iter()
                .filter_map(|id| grouping.boxes.iter().find(|b| &b.id == id))
                .map(|b| json!({ "id": b.id, "model": b.model, "kind": b.kind, "score": b.score }))
                .collect();
            let line = json!({
                "name": name,
                "chapter": chapter,
                "sourceIdx": source_idx,
                "page": rel,
                "group": group.id,
                "origin": group.origin,
                "disposition": group.disposition,
                "bounds": [bounds.x + ox, bounds.y + oy, bounds.w, bounds.h],
                "view": [view.x, view.y, view.w, view.h],
                "letteringPixels": group.lettering_pixels,
                "estimated": group.estimated,
                "reasons": group.reasons,
                "split": group.split.is_some(),
                "flaggedLarge": region.flagged_large,
                "boxes": boxes,
                "boxScore": group.score,
                "pixelModel": grouping.pixel_model,
                "models": group.models,
                "samOnly": group.models.iter().all(|m| *m == EvidenceModel::SamTsL),
                "detected": format!("{detected:?}"),
                "paper": format!("{interior:?}"),
                "inBalloon": inside,
                "script": verdict_name(&script),
                "gate": verdict_name(&gated),
                "readerPath": path,
                "final": verdict_name(&after),
                "cleans": after.cleans(),
                "hayai": match &reading {
                    Ok(r) => json!({
                        "text": r.text, "confidence": r.confidence, "floor": r.floor, "letters": r.letters(),
                        "isText": r.is_text(), "isLettering": r.is_lettering(),
                    }),
                    Err(e) => json!({ "error": e.to_string() }),
                },
                "readMs": read_ms,
            });
            lines.push_str(&line.to_string());
            lines.push('\n');
        }
        eprintln!("{chapter} page {source_idx}: {} groups", grouping.groups.len());
    }
    std::fs::write(out.join(format!("{chapter}.jsonl")), lines)?;
    Ok(())
}

fn verdict_name(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Clean { script } => format!("Clean({script})"),
        Verdict::NotJapanese { script } => format!("NotJapanese({script})"),
        other => format!("{other:?}"),
    }
}
