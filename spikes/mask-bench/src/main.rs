//! Which detection models should do what, scored on chapters a person finished.
//!
//! ```text
//! spike-mask-bench score --models DIR --out DIR <copy/cN.mtclean>...
//! spike-mask-bench time  --models DIR --out DIR --model ctd|small|full|sam|hayai [--every N] <copy/cN.mtclean>...
//! spike-mask-bench pick  --models DIR --out DIR --list rows.json
//! ```
//!
//! Reads **copies** of library chapters (`Job::open` sweeps files). A page's
//! evidence file holds the SAM-TS-L mask and the Ogkalu Full and CTD boxes of its
//! detection, so each combination's grouping is made again from them; the small
//! Ogkalu and CTD's own segmentation are run here, once a page.
//!
//! **The answer key is what the person left**: every visible model or hand mask
//! patch of a finished chapter, its lettering and applied mask. It favours the
//! combination that proposed those masks, since nobody deletes a harmless extra
//! mask or splits a merged one that cleaned well; read the numbers with that in
//! mind.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use cleaner_core::accel::{Accelerator, Preference};
use cleaner_core::balloon::{self, BalloonBox, BalloonClass, BalloonDetector};
use cleaner_core::detect::{DetBox, DetectedLanguage, Detector, Letterbox, Segmentation};
use cleaner_core::gate::hayai::Hayai;
use cleaner_core::gate::{self, LatinText};
use cleaner_core::image::{Raster, decode};
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::patch::Engine;
use cleaner_core::project::{Job, buffers};
use cleaner_core::rt_regions::FullRegions;
use cleaner_core::sam_ts::{Cancellation, SamTsSession};
use cleaner_core::text_groups::{self, BoxKind, Disposition, EvidenceFile, EvidenceModel, GroupOrigin, Grouping, UnitAnswer};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq)]
enum Ogkalu { None, Small, Full }

#[derive(Clone, Copy, PartialEq)]
enum Read { None, Units, Everything }

#[derive(Clone, Copy)]
struct Combo { name: &'static str, ogkalu: Ogkalu, ctd: bool, sam: bool, read: Read }

const fn combo(name: &'static str, ogkalu: Ogkalu, ctd: bool, sam: bool, read: Read) -> Combo {
    Combo { name, ogkalu, ctd, sam, read }
}

const COMBOS: &[Combo] = &[
    combo("SAM+Full+CTD no reader", Ogkalu::Full, true, true, Read::None),
    combo("SAM+Full+CTD (today)", Ogkalu::Full, true, true, Read::Units),
    combo("SAM+Full+CTD read all", Ogkalu::Full, true, true, Read::Everything),
    combo("SAM+Full", Ogkalu::Full, false, true, Read::Units),
    combo("SAM+Full read all", Ogkalu::Full, false, true, Read::Everything),
    combo("SAM+Small+CTD", Ogkalu::Small, true, true, Read::Units),
    combo("SAM+Small", Ogkalu::Small, false, true, Read::Units),
    combo("SAM+Small read all", Ogkalu::Small, false, true, Read::Everything),
    combo("SAM+CTD", Ogkalu::None, true, true, Read::Units),
    combo("SAM alone", Ogkalu::None, false, true, Read::Units),
    combo("SAM alone read all", Ogkalu::None, false, true, Read::Everything),
    combo("Full+CTD (Normal)", Ogkalu::Full, true, false, Read::Units),
    combo("Small+CTD (Normal)", Ogkalu::Small, true, false, Read::Units),
    combo("CTD alone", Ogkalu::None, true, false, Read::Units),
];

const HAYAI: [&str; 3] = ["hayai-ocr-vision.onnx", "hayai-ocr-decoder.onnx", "hayai-ocr-tokenizer.json"];

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("score or time")?;
    let (mut models, mut out, mut model, mut every, mut chapters, mut list) = (None, None, None, 15usize, Vec::new(), None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--models" => models = args.next().map(PathBuf::from),
            "--out" => out = args.next().map(PathBuf::from),
            "--model" => model = args.next(),
            "--every" => every = args.next().context("--every")?.parse()?,
            "--list" => list = args.next().map(PathBuf::from),
            _ => chapters.push(PathBuf::from(arg)),
        }
    }
    let (models, out) = (models.context("--models")?, out.context("--out")?);
    std::fs::create_dir_all(&out)?;
    match mode.as_str() {
        "score" => score(&models, &out, every, &chapters),
        "pick" => pick(&models, &out, &list.context("--list")?),
        "time" => time(&models, &out, &model.context("--model")?, every, &chapters),
        _ => Err(anyhow!("score or time")),
    }
}

/* ------------------------------------------------------------------ */

struct Page { source_idx: usize, raster: Raster, evidence: EvidenceFile }

/// The pages of a chapter with one whole-page evidence file, the file most of
/// its rows name.
fn pages_of(job: &Job, manifest: &Path) -> Result<Vec<(usize, PathBuf, String)>> {
    let mut named: BTreeMap<usize, BTreeMap<String, u32>> = BTreeMap::new();
    for row in &job.project.detections {
        if let Some(reference) = row.group.as_ref().and_then(|group| group.evidence_ref.clone()) {
            *named.entry(row.source_idx).or_default().entry(reference).or_default() += 1;
        }
    }
    for row in job.project.patches.iter().chain(&job.project.withdrawn_patches) {
        if let Some(reference) = row.provenance.params_snapshot["group"]["evidence"].as_str() {
            *named.entry(row.source_idx).or_default().entry(reference.to_owned()).or_default() += 1;
        }
    }
    let project = manifest.parent().context("manifest has no folder")?;
    Ok(named.into_iter().filter_map(|(source_idx, refs)| {
        let reference = refs.into_iter().max_by_key(|(_, count)| *count)?.0;
        let rel = &job.project.sources.get(source_idx)?.rel_path;
        Some((source_idx, project.join(rel), reference))
    }).collect())
}

fn load_page(job: &Job, (source_idx, path, reference): &(usize, PathBuf, String)) -> Result<Option<Page>> {
    let raster = decode(&std::fs::read(path).with_context(|| path.display().to_string())?).map_err(|e| anyhow!("{e}"))?;
    let evidence = EvidenceFile::decode(&std::fs::read(job.sidecar().join(reference))?).map_err(|e| anyhow!("{e}"))?;
    let stored = &evidence.grouping;
    let whole = stored.origin == (0, 0) && (stored.width, stored.height) == (raster.width, raster.height);
    Ok(whole.then_some(Page { source_idx: *source_idx, raster, evidence }))
}

/// What the person left on a page: each kept patch's lettering, by patch, and
/// the union of applied masks.
struct Truth { owner: Vec<u16>, applied: Vec<bool>, ink_of: Vec<u64>, hand: Vec<bool> }

fn truth_of(job: &Job, source_idx: usize, w: u32, h: u32) -> Result<Truth> {
    let (wu, hu) = (w as usize, h as usize);
    let mut truth = Truth { owner: vec![0; wu * hu], applied: vec![false; wu * hu], ink_of: vec![0], hand: vec![false] };
    let paint = |mask: &Mask, mut set: Box<dyn FnMut(usize) + '_>| {
        let b = mask.bounds;
        for y in b.y.max(0)..b.bottom().min(i64::from(h)) {
            for x in b.x.max(0)..b.right().min(i64::from(w)) {
                if mask.contains(x, y) { set(y as usize * wu + x as usize); }
            }
        }
    };
    for record in job.project.patches.iter().filter(|row| row.source_idx == source_idx && row.visible) {
        if matches!(record.provenance.engine, Engine::Paint | Engine::Clone) { continue; }
        let dir = job.sidecar();
        let Ok(mask) = std::fs::read(dir.join(&record.mask_ref)).map_err(|e| e.to_string())
            .and_then(|bytes| buffers::decode_mask(&bytes).map_err(|e| e.to_string())) else { continue };
        let ink = std::fs::read(dir.join(record.ink_ref())).ok()
            .and_then(|bytes| buffers::decode_mask(&bytes).ok()).unwrap_or_else(|| mask.clone());
        let id = truth.ink_of.len() as u16;
        let mut pixels = 0u64;
        {
            let owner = &mut truth.owner;
            paint(&ink, Box::new(|at| { if owner[at] == 0 { owner[at] = id; pixels += 1; } }));
        }
        truth.ink_of.push(pixels);
        truth.hand.push(record.provenance.params_snapshot["source"].as_str() == Some("hand"));
        let applied = &mut truth.applied;
        paint(&mask, Box::new(|at| applied[at] = true));
    }
    Ok(truth)
}

#[derive(Default)]
struct Tally {
    pages: u64, truth_units: u64, truth_units_covered: u64, truth_ink: u64, truth_ink_covered: u64,
    clean_px: u64, clean_px_kept: u64, groups: u64, art_groups: u64, bleed_groups: u64, bridge_groups: u64,
    held: u64, reads: u64, inside: u64, mask_only: u64, truth_hand_units: u64, truth_hand_covered: u64,
}

fn score(models: &Path, out: &Path, every: usize, chapters: &[PathBuf]) -> Result<()> {
    let mut reader = Hayai::open(&models.join(HAYAI[0]), &models.join(HAYAI[1]), &models.join(HAYAI[2]), Preference::Automatic)
        .map_err(|e| anyhow!("{e}"))?;
    let mut small = BalloonDetector::open(&models.join("comic-text-and-bubble-detector-detector-v4-s_int8.onnx"), Preference::Automatic)
        .map_err(|e| anyhow!("{e}"))?;
    let mut ctd_model = Detector::open(&models.join("comictextdetector.onnx"), Preference::Automatic).map_err(|e| anyhow!("{e}"))?;
    let mut tallies: Vec<Tally> = COMBOS.iter().map(|_| Tally::default()).collect();
    let mut lines = String::new();
    for manifest in chapters {
        let job = Job::open(manifest).map_err(|e| anyhow!("{e}"))?;
        let chapter = manifest.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        for spec in pages_of(&job, manifest)?.into_iter().step_by(every) {
            let Some(page) = load_page(&job, &spec)? else { continue };
            let stored = &page.evidence.grouping;
            let (w, h) = (stored.width, stored.height);
            let (mut full, mut ctd) = (Vec::new(), Vec::new());
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
                full.push(BalloonBox { rect: item.rect, class, score: item.score });
            }
            if full.is_empty() || ctd.is_empty() || page.evidence.mask.is_empty() { continue; }
            let small_boxes = small.detect(&page.raster).map_err(|e| anyhow!("{e}"))?;
            let detection = ctd_model.detect(&page.raster).map_err(|e| anyhow!("{e}"))?;
            let ctd_seg = detection.segmentation.lettering_mask();
            let truth = truth_of(&job, page.source_idx, w, h)?;
            if truth.ink_of.len() <= 1 { continue; }
            let mut asked: HashMap<(i64, i64, u32, u32), UnitAnswer> = HashMap::new();

            for (at, combo) in COMBOS.iter().enumerate() {
                let rt: &[BalloonBox] = match combo.ogkalu { Ogkalu::None => &[], Ogkalu::Small => &small_boxes, Ogkalu::Full => &full };
                let ctd_boxes: &[DetBox] = if combo.ctd { &ctd } else { &[] };
                let mask: Option<&[u8]> = if combo.sam { Some(&page.evidence.mask) } else if combo.ctd { Some(&ctd_seg) } else { None };
                let mut inputs = text_groups::Inputs::new(w, h, mask);
                inputs.pixel_model = if combo.sam { EvidenceModel::SamTsL } else { EvidenceModel::Ctd };
                inputs.page = Some(&page.raster);
                inputs.rt = rt;
                inputs.ctd = ctd_boxes;
                inputs.text_boxes_only = !combo.sam && combo.ogkalu != Ogkalu::None;
                let mut reads = 0u64;
                let raster = &page.raster;
                let mut ask = |rect: Rect, reads: &mut u64| {
                    let rect = cleaner_core::detect::tight_rect(rect, w, h);
                    *reads += 1;
                    *asked.entry((rect.x, rect.y, rect.w, rect.h)).or_insert_with(|| {
                        gate::unit_answer(reader.read(raster, rect).ok().as_ref(), LatinText::Leave)
                    })
                };
                let grouping: Grouping = if combo.read == Read::None {
                    text_groups::group(&inputs).map_err(|e| anyhow!("{e}"))?
                } else {
                    let mut check = |rect: Rect| ask(rect, &mut reads);
                    text_groups::group_checked(&inputs, Some(&mut check)).map_err(|e| anyhow!("{e}"))?
                };
                let seg = Segmentation { width: w, height: h, levels: mask.map_or_else(|| vec![0; (w * h) as usize], <[u8]>::to_vec), fit: Letterbox::fit(w, h) };
                let tally = &mut tallies[at];
                tally.pages += 1;
                let mut covered = vec![0u64; truth.ink_of.len()];
                let mut group_rows = Vec::new();
                for group in &grouping.groups {
                    if group.disposition != Disposition::Clean {
                        tally.held += 1;
                        continue;
                    }
                    if combo.read == Read::Everything && ask(group.bounds, &mut reads) != UnitAnswer::Text {
                        tally.held += 1;
                        continue;
                    }
                    let lettering = grouping.lettering(group);
                    let (mut px, mut kept) = (0u64, 0u64);
                    let mut touched: BTreeMap<u16, u64> = BTreeMap::new();
                    let b = lettering.bounds;
                    for y in b.y.max(0)..b.bottom().min(i64::from(h)) {
                        for x in b.x.max(0)..b.right().min(i64::from(w)) {
                            if !lettering.contains(x, y) { continue; }
                            let at = y as usize * w as usize + x as usize;
                            px += 1;
                            if truth.applied[at] { kept += 1; }
                            let owner = truth.owner[at];
                            if owner != 0 { covered[owner as usize] += 1; *touched.entry(owner).or_default() += 1; }
                        }
                    }
                    if px == 0 { continue; }
                    let inside = balloon::in_bubble(raster, &seg, group.bounds, group.bounds, rt);
                    tally.groups += 1;
                    tally.clean_px += px;
                    tally.clean_px_kept += kept;
                    tally.inside += u64::from(inside);
                    tally.mask_only += u64::from(group.origin == GroupOrigin::MaskLayout);
                    let share = kept as f64 / px as f64;
                    let art = share < 0.10;
                    let bleed = !art && share < 0.85 && px - kept >= 150;
                    // One group across two masks the person kept apart, each a real share of it.
                    let bridge = touched.iter().filter(|(owner, n)| {
                        **n as f64 >= 0.1 * truth.ink_of[**owner as usize] as f64 && **n >= 100
                    }).count() >= 2;
                    tally.art_groups += u64::from(art);
                    tally.bleed_groups += u64::from(bleed);
                    tally.bridge_groups += u64::from(bridge);
                    group_rows.push(json!({
                        "bounds": [group.bounds.x, group.bounds.y, group.bounds.w, group.bounds.h],
                        "px": px, "kept": kept, "inside": inside, "art": art, "bleed": bleed, "bridge": bridge,
                        "origin": group.origin,
                    }));
                }
                for id in 1..truth.ink_of.len() {
                    if truth.ink_of[id] == 0 { continue; }
                    // Kept ink is the lettering grown by the fit, several times wider
                    // than a model's pixels, so a fifth of it is a mask found.
                    let hit = covered[id] as f64 >= 0.2 * truth.ink_of[id] as f64;
                    tally.truth_units += 1;
                    tally.truth_ink += truth.ink_of[id];
                    tally.truth_ink_covered += covered[id].min(truth.ink_of[id]);
                    tally.truth_units_covered += u64::from(hit);
                    if truth.hand[id] {
                        tally.truth_hand_units += 1;
                        tally.truth_hand_covered += u64::from(hit);
                    }
                }
                tally.reads += reads;
                lines.push_str(&json!({ "chapter": chapter, "source": page.source_idx, "combo": combo.name, "reads": reads, "groups": group_rows }).to_string());
                lines.push('\n');
            }
            eprintln!("{chapter} page {} done", page.source_idx);
        }
    }
    std::fs::write(out.join("groups.jsonl"), lines)?;
    let summary: Vec<Value> = COMBOS.iter().zip(&tallies).map(|(combo, t)| {
        let share = |a: u64, b: u64| if b == 0 { 0.0 } else { (a as f64 / b as f64 * 1000.0).round() / 10.0 };
        json!({
            "combo": combo.name, "pages": t.pages,
            "groups": t.groups, "masksMissedPct": 100.0 - share(t.truth_units_covered, t.truth_units),
            "keptInkFoundPct": share(t.truth_ink_covered, t.truth_ink),
            "extraPixelsPct": 100.0 - share(t.clean_px_kept, t.clean_px),
            "artGroups": t.art_groups, "bleedGroups": t.bleed_groups, "bridgeGroups": t.bridge_groups,
            "held": t.held, "readsPerPage": share(t.reads, t.pages) / 100.0, "insidePct": share(t.inside, t.groups),
            "maskOnly": t.mask_only, "handMasksFoundPct": share(t.truth_hand_covered, t.truth_hand_units),
            "keptMasks": t.truth_units,
        })
    }).collect();
    std::fs::write(out.join("summary.json"), serde_json::to_string_pretty(&summary)?)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

/* ------------------------------------------------------------------ */

fn time(models: &Path, out: &Path, model: &str, every: usize, chapters: &[PathBuf]) -> Result<()> {
    let mut pages = Vec::new();
    for manifest in chapters {
        let job = Job::open(manifest).map_err(|e| anyhow!("{e}"))?;
        for spec in pages_of(&job, manifest)?.into_iter().step_by(every) {
            if let Some(page) = load_page(&job, &spec)? { pages.push(page); }
        }
    }
    let opened = Instant::now();
    let cancel = Cancellation::default();
    let mut run: Box<dyn FnMut(&Page) -> Result<usize>> = match model {
        "small" => {
            let mut m = BalloonDetector::open(&models.join("comic-text-and-bubble-detector-detector-v4-s_int8.onnx"), Preference::Automatic).map_err(|e| anyhow!("{e}"))?;
            Box::new(move |p| { m.detect(&p.raster).map_err(|e| anyhow!("{e}"))?; Ok(1) })
        }
        "ctd" => {
            let mut m = Detector::open(&models.join("comictextdetector.onnx"), Preference::Automatic).map_err(|e| anyhow!("{e}"))?;
            Box::new(move |p| { m.detect(&p.raster).map_err(|e| anyhow!("{e}"))?; Ok(1) })
        }
        "full" => {
            let (mut m, _) = FullRegions::open(&models.join("detector.onnx"), Preference::Automatic).map_err(|e| anyhow!("{e}"))?;
            let cancel = Cancellation::default();
            Box::new(move |p| { m.detect_halves_cancellable(&p.raster, &cancel).map_err(|e| anyhow!("{e}"))?; Ok(2) })
        }
        "sam" => {
            let mut m = SamTsSession::open_on(&models.join("sam-ts-l"), Accelerator::WebGpu, &cancel).map_err(|e| anyhow!("{e}"))?;
            let cancel = Cancellation::default();
            Box::new(move |p| { m.infer(&p.raster, &cancel).map_err(|e| anyhow!("{e}"))?; Ok(1) })
        }
        "hayai" | "hayai-cpu" => {
            let preference = if model == "hayai" { Preference::Automatic } else { Preference::CpuOnly };
            let mut m = Hayai::open(&models.join(HAYAI[0]), &models.join(HAYAI[1]), &models.join(HAYAI[2]), preference).map_err(|e| anyhow!("{e}"))?;
            eprintln!("hayai on {:?}", m.selection().accelerator);
            Box::new(move |p| {
                let mut reads = 0;
                for group in p.evidence.grouping.groups.iter().filter(|g| g.disposition == Disposition::Clean) {
                    let rect = cleaner_core::detect::tight_rect(group.bounds, p.raster.width, p.raster.height);
                    let _ = m.read(&p.raster, rect);
                    reads += 1;
                }
                Ok(reads)
            })
        }
        _ => return Err(anyhow!("unknown model {model}")),
    };
    let open_ms = opened.elapsed().as_secs_f64() * 1000.0;
    let mut times: Vec<f64> = Vec::new();
    let mut units = 0usize;
    for page in &pages {
        let started = Instant::now();
        units += run(page)?;
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let first = times.first().copied().unwrap_or(0.0);
    let mut rest = times[1.min(times.len())..].to_vec();
    rest.sort_by(f64::total_cmp);
    let median = rest.get(rest.len() / 2).copied().unwrap_or(first);
    let line = json!({ "model": model, "pages": pages.len(), "openMs": open_ms, "firstPageMs": first, "medianPageMs": median, "units": units });
    println!("{line}");
    std::fs::write(out.join(format!("time-{model}.json")), line.to_string())?;
    Ok(())
}

/* ------------------------------------------------------------------ */

/// What decides Fill or a model: for each listed box, the balloon answer, the
/// paper walk and the plain numbers of the paper round its ink. Reads page files
/// and runs CTD; opens no chapter, so a list may name the real library.
fn pick(models: &Path, out: &Path, list: &Path) -> Result<()> {
    let rows: Vec<Value> = serde_json::from_slice(&std::fs::read(list)?)?;
    let mut ctd = Detector::open(&models.join("comictextdetector.onnx"), Preference::Automatic).map_err(|e| anyhow!("{e}"))?;
    let mut small = BalloonDetector::open(&models.join("comic-text-and-bubble-detector-detector-v4-s_int8.onnx"), Preference::Automatic).map_err(|e| anyhow!("{e}"))?;
    let mut cached: Option<(String, Raster, Segmentation, Vec<BalloonBox>)> = None;
    let mut lines = String::new();
    for row in &rows {
        let path = row["page"].as_str().context("page")?.to_owned();
        if cached.as_ref().is_none_or(|(at, ..)| *at != path) {
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let Ok(raster) = decode(&bytes) else { continue };
            let seg = ctd.detect(&raster).map_err(|e| anyhow!("{e}"))?.segmentation;
            let boxes = small.detect(&raster).map_err(|e| anyhow!("{e}"))?;
            cached = Some((path.clone(), raster, seg, boxes));
        }
        let (_, raster, seg, boxes) = cached.as_ref().unwrap();
        let b = &row["bbox"];
        let rect = Rect::new(b["x"].as_i64().context("x")?, b["y"].as_i64().context("y")?, b["w"].as_u64().context("w")? as u32, b["h"].as_u64().context("h")? as u32);
        let interior = balloon::interior_of(raster, seg, rect);
        let solid = matches!(interior, balloon::Interior::Solid { .. });
        // The paper between and round the strokes: every pixel of the grown box
        // CTD does not call lettering, grown a little off the ink.
        let area = rect.grown(8, raster.width, raster.height);
        let mut levels: Vec<u16> = Vec::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let near = (-3i64..=3).any(|dy| (-3i64..=3).any(|dx| {
                    (x + dx) >= 0 && (y + dy) >= 0 && seg.is_text((x + dx) as u32, (y + dy) as u32)
                }));
                if !near { levels.push(raster.luma16_at(x as u32, y as u32) >> 8); }
            }
        }
        levels.sort_unstable();
        let at = |share: f64| levels.get(((levels.len().max(1) - 1) as f64 * share) as usize).copied().unwrap_or(0);
        let median = at(0.5);
        let close = levels.iter().filter(|&&level| level.abs_diff(median) <= 12).count() as f64 / levels.len().max(1) as f64;
        lines.push_str(&json!({
            "label": row["label"], "id": row["id"], "inside": balloon::detected(rect, boxes).inside(), "solid": solid,
            "interior": format!("{interior:?}"), "p5": at(0.05), "p50": median, "p95": at(0.95), "close": close, "samples": levels.len(),
        }).to_string());
        lines.push('\n');
    }
    std::fs::write(out.join("pick.jsonl"), lines)?;
    Ok(())
}
