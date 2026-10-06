//! Every engine that produces pixels, on the same regions, **one session at a
//! time**.
//!
//! ```text
//! spike-clean-page <page.png> --compare-engines [--out-dir DIR]
//! ```
//!
//! Two rungs produce pixels today - rung 0 [`fill`] and rung 2 [`lama`];
//! Denoise fill's rung 1 and MI-GAN's rung 3 are gone from the ladder - and there is no image anywhere in this
//! repository that shows what they do to the same region. This makes one: a
//! contact sheet whose rows are regions and whose columns are the page, then
//! each rung in order, every crop labelled with the rung, its latency and its
//! residue count.
//!
//! ## Why this is three phases with hard drops between them
//!
//! The obvious shape - open everything, loop over regions, write images - is the
//! shape the earlier `compare_rungs` had, and it was wrong for three separate
//! reasons rather than for tidiness.
//!
//! **It can abort the process.** Two WebGPU sessions live at once produce
//! *"A command encoder is already
//! encoding to this command buffer"*, or `-[_MTLCommandEncoder dealloc]` failing
//! its `Command encoder released without endEncoding` assertion, and SIGABRT.
//! [`Preference::Automatic`] selects WebGPU for both model rungs on an M5, so
//! holding two model sessions together (as rung 2 and the removed rung 3 once
//! were) is one timing accident away from losing the run.
//!
//! **It cannot report memory honestly.** The detector alone is ~195 MB of
//! session and ~1.2 GB of ONNX Runtime arena, a held LaMa session is
//! ~350 MB, MI-GAN's is ~20 MB with its own arena beside it. A
//! harness that stacks five allocators and then prints an RSS number is printing
//! the sum of its own mistakes.
//!
//! **It is not what rule 6 asks
//! of the application.** One thing in flight, buffers dropped before the next
//! begins, is the discipline the whole memory budget rests on, and a comparison
//! that violates it measures something the shipping code never does.
//!
//! So: **Phase A** opens the three detection sessions, produces one
//! [`fit::Fitted`] per gated region, and drops all three before any engine
//! opens. **Phase B** runs the four rungs in order, each model rung opening its
//! session, rendering every region, and dropping it before the next rung is
//! reached. **Phase C** composes and writes the images, with no session alive at
//! all.
//!
//! ## Why two engines cannot be held here by accident
//!
//! Not by discipline - by the return types. [`with_lama`] (and the removed
//! `with_migan` before it) opens its session as a **local** and returns `Vec<Option<Shot>>`,
//! which contains rasters and no session. There is no binding in [`run`] that
//! could hold an [`lama::Inpainter`], so no later edit can accidentally extend
//! one's life across the other's open; the compiler rejects the attempt rather
//! than a reviewer catching it. The explicit `drop` before each function's final
//! RSS sample is not what provides the guarantee - it is what makes the sample
//! land *after* the release rather than at the closing brace.
//!
//! ## What this may not conclude
//!
//! **No winner is declared between rungs 2 and 3.** A quality
//! comparison wants a licence-cleared corpus and synthetic fixtures
//! are forbidden from standing in for one. Residue counts and visible defects are
//! observations, and they are reported as such; a ranking would be a claim this
//! corpus cannot carry - every rung-2 region in this
//! repository is a flat-paper dialogue balloon that arrived through a
//! fallback with a fixture defect underneath it.
//!
//! ## The residue number
//!
//! **Pixels more than 64 levels below the region's own paper**, counted over
//! `mask ⊕ ISOLATION_RADIUS` - the set both model rungs are permitted to write
//! through ([`engines::model::applied_mask`]), so the same set is counted for
//! all three columns including the untouched page.
//!
//! It is not summed darkness: "16-30% retained ink" was
//! measured that way and the quantity is dominated by rung 2's fill sitting
//! about one level under the surrounding paper. r9 read 36.4% of its box
//! by that measure with 149 pixels actually dark and the balloon visually clean.
//! A threshold well below the paper counts marks; a sum counts the offset.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use cleaner_core::accel::Preference;
use cleaner_core::composite::composite;
use cleaner_core::detect::Detector;
use cleaner_core::engines::{fill, lama, model};
use cleaner_core::fit::{self, EdgeMap};
use cleaner_core::gate::ScriptGate;
use cleaner_core::image::{Format, Raster, encode};
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::patch::{Engine, Patch, Provenance};
use cleaner_core::quality;

use crate::paint;

/* ------------------------------------------------------------------ */
/* What a region and a rendering are, here                             */
/* ------------------------------------------------------------------ */

/// One gated region, reduced to the two things the engines need.
///
/// Everything else the detection phase produced - the segmentation, the balloon
/// boxes, the edge map - is deliberately **not** in this struct, because the
/// point of Phase A is that it can be dropped whole. A field here is a field
/// that survives into Phase B.
struct Fit {
    /// `r00`, `r01`, … in the order the detector emitted them, restricted to the
    /// regions the gate cleared. It is the label on the contact sheet and in the
    /// table, so the two can be read against each other.
    label: String,
    fitted: fit::Fitted,
}

/// What one engine produced for one region.
struct Shot {
    mask: Mask,
    pixels: Raster,
    elapsed: Duration,
    /// How many model runs the region cost. `None` on the arithmetic rungs,
    /// which have no such notion.
    tiles: Option<u32>,
    /// A rung's own refusal, as the catalogue key it carries. Present only when
    /// there are no pixels.
    declined: Option<String>,
}

impl Shot {
    fn made(mask: Mask, pixels: Raster, elapsed: Duration, tiles: Option<u32>) -> Shot {
        Shot { mask, pixels, elapsed, tiles, declined: None }
    }
}

/// The three columns, in the order they are shown.
const COLUMNS: [&str; 3] = ["ORIGINAL", "R0 FILL", "R2 LAMA"];

/* ------------------------------------------------------------------ */
/* The run                                                             */
/* ------------------------------------------------------------------ */

pub fn run(models: &Path, page: &Raster, out_dir: &Path, stem: &str) -> Result<()> {
    let mut rss = Vec::new();
    sample(&mut rss, "before any session");

    println!("\n--- phase A: detection, then let it go ---");
    let noise = fit::page_noise_sigma(page);
    let fits = detect(models, page, &mut rss)?;
    if fits.is_empty() {
        println!(
            "no region of {stem} clears the script gate with a non-empty seed; \
             there is nothing for an engine to render and no sheet is written"
        );
        report(&rss);
        return Ok(());
    }
    println!("{} gated regions, page noise sigma {:.1}", fits.len(), noise);

    println!("\n--- phase B: one engine at a time ---");
    let rung0 = with_fill(page, &fits);
    sample(&mut rss, "rung 0 done (no session)");
    let rung2 = with_lama(models, page, &fits, &mut rss)?;

    println!("\n--- phase C: the images, with no session alive ---");
    let engines = [
        (Engine::Fill, &rung0),
        (Engine::Lama, &rung2),
    ];

    // The view each row is cropped to, and the magnification it is shown at.
    // Both are per region and shared by all three columns, so a row is three
    // pictures of the same rectangle at the same scale and a difference between
    // two of them is a difference in the pixels.
    let views: Vec<(Rect, u32)> = fits
        .iter()
        .map(|fit| {
            let view = fit.fitted.mask.bounds.grown(SURROUND, page.width, page.height);
            (view, magnification(view))
        })
        .collect();

    // One composited page in flight at a time, for rule 6's reason: five full
    // 8-bit RGB copies of a 1600×2400 page is 57 MB held for the length of a
    // loop that needs one of them at a time.
    let mut columns: Vec<Vec<Raster>> = Vec::new();
    let mut residues: Vec<Vec<u32>> = Vec::new();
    let (crops, ink) = crops_of(page, &fits, &views);
    columns.push(crops);
    residues.push(ink);
    for (engine, shots) in engines {
        let patches: Vec<Patch> = shots
            .iter()
            .enumerate()
            .filter_map(|(index, shot)| {
                let shot = shot.as_ref()?;
                Some(a_patch(index, engine, shot.mask.clone(), shot.pixels.clone()))
            })
            .collect();
        let composited = composite(page, &patches)?;
        let (crops, ink) = crops_of(&composited, &fits, &views);
        columns.push(crops);
        residues.push(ink);
    }
    sample(&mut rss, "every column composited");

    let table = table(&fits, &engines, &residues, page, noise);
    print!("{table}");
    let table_path = out_dir.join(format!("{stem}.engines.txt"));
    std::fs::write(&table_path, &table)?;
    println!("wrote {}", table_path.display());

    let sheet = sheet(&fits, &engines, &columns, &residues);
    let sheet_path = out_dir.join(format!("{stem}.engines.png"));
    std::fs::write(&sheet_path, encode(&sheet, Format::Png).map_err(|e| anyhow!("{e}"))?)?;
    println!(
        "wrote {} - {}×{}, rows are regions, columns are {}",
        sheet_path.display(),
        sheet.width,
        sheet.height,
        COLUMNS.join(" / ")
    );
    sample(&mut rss, "sheet written");

    report(&rss);
    Ok(())
}

/// How much page is shown around a region's mask, in native pixels.
///
/// Wide enough to carry the balloon's outline and some paper: rung 3's severed
/// arc and rung 2's invented marks are both defects *at the edge of the hole*,
/// and a crop tight to the mask shows neither against anything.
const SURROUND: u32 = 40;

/// The integer magnification a crop is shown at.
///
/// A residue mark is a few pixels wide (48 to 149 pixels for a whole
/// region), so a region shown at 1:1 in a grid is a smudge nobody can count.
/// Integer factors only, and nearest-neighbour, so what is magnified is the
/// pixels rather than an interpolation of them.
fn magnification(view: Rect) -> u32 {
    (320 / view.w.max(view.h).max(1)).clamp(1, 4)
}

/* ------------------------------------------------------------------ */
/* Phase A                                                             */
/* ------------------------------------------------------------------ */

/// Detection, the balloon pass, the gate and the fit - and then nothing.
///
/// The three sessions are locals of this function and the return type holds
/// none of them, so they are gone before the caller has a value. The explicit
/// drops are there to put the RSS sample after the release rather than at the
/// closing brace; without them the "after detection is dropped" row would be
/// taken with all three still alive.
fn detect(models: &Path, page: &Raster, rss: &mut Vec<(String, u64)>) -> Result<Vec<Fit>> {
    let (mut detector, detector_open) =
        timed(|| Detector::open(&models.join("comictextdetector.onnx"), Preference::Automatic))?;
    let (mut balloons_model, balloon_open) = timed(|| {
        cleaner_core::balloon::BalloonDetector::open(
            &models.join("comic-text-and-bubble-detector-detector-v4-s_int8.onnx"),
            Preference::Automatic,
        )
    })?;
    let (mut gate, gate_open) = timed(|| {
        ScriptGate::open(
            &models.join("image-script-identification-osd_lstm.onnx"),
            &models.join("image-script-identification-osd_labels.json"),
            Preference::Automatic,
        )
    })?;
    println!(
        "detector {} ({} ms), balloons {} ({} ms), script gate {} ({} ms)",
        provider(detector.selection()),
        detector_open.as_millis(),
        provider(balloons_model.selection()),
        balloon_open.as_millis(),
        provider(gate.selection()),
        gate_open.as_millis(),
    );
    sample(rss, "three detection sessions open");

    let (detection, detect_ms) = timed(|| detector.detect(page))?;
    let (balloons, balloon_ms) = timed(|| balloons_model.detect(page))?;
    println!("detect {} ms, balloons {} ms", detect_ms.as_millis(), balloon_ms.as_millis());
    sample(rss, "detection run (arena paid)");

    let regions =
        cleaner_core::detect::build_regions(detection.boxes.clone(), page.width, page.height);
    let scale = detection.segmentation.proxy_scale();
    let noise = fit::page_noise_sigma(page);
    let edges = EdgeMap::sobel(page);

    let mut fits = Vec::new();
    let mut skipped = 0u32;
    for region in regions.iter() {
        let detected = cleaner_core::balloon::detected(region.masking, &balloons);
        let inside = cleaner_core::balloon::in_bubble(
            page,
            &detection.segmentation,
            region.text_bounds(),
            region.masking,
            &balloons,
        );
        let verdict = gate.judge(
            page,
            &detection.segmentation,
            region,
            detected,
            inside,
            cleaner_core::gate::OutsideText::Review,
        )?;
        if !verdict.cleans() {
            skipped += 1;
            continue;
        }
        let seed = fit::seed_mask(&detection.segmentation, region, page.width, page.height);
        if seed.is_empty() {
            skipped += 1;
            continue;
        }
        let fitted = fit::fit(page, &seed, scale, noise, &edges, false);
        fits.push(Fit { label: format!("r{:02}", fits.len()), fitted });
    }
    println!("{} regions detected, {} gate-skipped or empty-seeded", regions.len(), skipped);

    drop(detector);
    drop(balloons_model);
    drop(gate);
    drop(detection);
    drop(edges);
    sample(rss, "detection dropped");
    Ok(fits)
}

/* ------------------------------------------------------------------ */
/* Phase B                                                             */
/* ------------------------------------------------------------------ */

/// Rung 0. No session, and no way for one region to affect another.
fn with_fill(page: &Raster, fits: &[Fit]) -> Vec<Option<Shot>> {
    fits.iter()
        .map(|fit| {
            let started = Instant::now();
            let pixels = fill::render(page, &fit.fitted);
            Some(Shot::made(fit.fitted.mask.clone(), pixels, started.elapsed(), None))
        })
        .collect()
}

/// Rung 2: open, render every region, drop.
///
/// The session is a local and the return type is rasters. See the module docs:
/// this signature is what makes "never two engines at once" a property of the
/// code rather than a thing to remember.
fn with_lama(
    models: &Path,
    page: &Raster,
    fits: &[Fit],
    rss: &mut Vec<(String, u64)>,
) -> Result<Vec<Option<Shot>>> {
    let (mut inpainter, open) =
        timed(|| lama::Inpainter::open(&models.join("lama-manga.onnx"), Preference::Automatic))?;
    println!(
        "rung 2 {} - session built in {} ms",
        provider(inpainter.selection()),
        open.as_millis()
    );
    sample(rss, "rung 2 open");

    let mut shots = Vec::with_capacity(fits.len());
    for fit in fits {
        let started = Instant::now();
        shots.push(match inpainter.render(page, &fit.fitted) {
            Ok(rendered) => Some(Shot::made(
                rendered.mask,
                rendered.pixels,
                started.elapsed(),
                Some(rendered.tiles),
            )),
            Err(lama::Error::Declined(declined)) => Some(Shot {
                mask: Mask::empty(fit.fitted.mask.bounds),
                pixels: paint::blank(1, 1),
                elapsed: started.elapsed(),
                tiles: None,
                declined: Some(declined.reason_key().into()),
            }),
            Err(lama::Error::Run(fault)) => {
                return Err(anyhow!("rung 2 on {}: {fault}", fit.label));
            }
        });
    }
    sample(rss, "rung 2 rendered every region");

    drop(inpainter);
    sample(rss, "rung 2 dropped");
    Ok(shots)
}

fn crops_of(page: &Raster, fits: &[Fit], views: &[(Rect, u32)]) -> (Vec<Raster>, Vec<u32>) {
    let viewable = paint::rgb8(page);
    let mut crops = Vec::with_capacity(fits.len());
    let mut ink = Vec::with_capacity(fits.len());
    for (fit, (view, factor)) in fits.iter().zip(views) {
        crops.push(paint::upscaled(&paint::crop(&viewable, *view), *factor));
        ink.push(residue(page, &fit.fitted));
    }
    (crops, ink)
}

/// How much ink is left where an engine was allowed to write.
///
/// See the module docs for why this is a count and not a sum. The threshold is
/// 64 levels on an 8-bit scale below the region's **own** ring median, so a
/// grey-toned panel and a white one are judged against their own paper rather
/// than against a page-wide constant.
fn residue(page: &Raster, fitted: &fit::Fitted) -> u32 {
    let window = model::applied_mask(fitted, page.width, page.height);
    // `RingStats::median` is 16-bit luma; 257 is the conversion the rest of the
    // codebase uses for the same purpose (`fill::tones_agree`).
    let paper = (fitted.ring.median / 257) as u8;
    let floor = paper.saturating_sub(64);
    let bounds = window.bounds;
    let mut count = 0;
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if x < 0 || y < 0 || x >= page.width as i64 || y >= page.height as i64 {
                continue;
            }
            if !window.contains(x, y) {
                continue;
            }
            if paint::luma8(page, x as u32, y as u32) < floor {
                count += 1;
            }
        }
    }
    count
}

/// The contact sheet: rows are regions, columns are the page and then the four
/// rungs in order.
fn sheet(
    fits: &[Fit],
    engines: &[(Engine, &Vec<Option<Shot>>)],
    columns: &[Vec<Raster>],
    residues: &[Vec<u32>],
) -> Raster {
    let mut rows: Vec<Raster> = Vec::with_capacity(fits.len());
    for (index, fit) in fits.iter().enumerate() {
        let mut cells: Vec<Raster> = Vec::with_capacity(COLUMNS.len());
        cells.push(paint::labelled(
            &format!(
                "{} {} {} ROUTE {} INK {}",
                fit.label.to_uppercase(),
                COLUMNS[0],
                extent(&fit.fitted),
                route_word(fit.fitted.route),
                residues[0][index],
            ),
            &columns[0][index],
        ));
        for (column, (_, shots)) in engines.iter().enumerate() {
            let label = match shots[index].as_ref() {
                Some(shot) => match &shot.declined {
                    Some(key) => format!("{} DECLINED {}", COLUMNS[column + 1], upper(key)),
                    None => format!(
                        "{} {}MS{} INK {}",
                        COLUMNS[column + 1],
                        shot.elapsed.as_millis(),
                        shot.tiles.map(|t| format!(" {t} TILES")).unwrap_or_default(),
                        residues[column + 1][index],
                    ),
                },
                None => format!("{} MISSING", COLUMNS[column + 1]),
            };
            cells.push(paint::labelled(&label, &columns[column + 1][index]));
        }
        rows.push(paint::beside(&cells.iter().collect::<Vec<_>>()));
    }
    paint::stacked(&rows.iter().collect::<Vec<_>>())
}

/// The same numbers as text, because a picture is not a number anybody can
/// paste into a note.
fn table(
    fits: &[Fit],
    engines: &[(Engine, &Vec<Option<Shot>>)],
    residues: &[Vec<u32>],
    page: &Raster,
    noise: f32,
) -> String {
    let mut out = String::new();
    out.push_str(
        "\nregion  extent                  route    | rung        ms  tiles   residue  quality\n\
           ------  ----------------------  -------  | ----------  ----  -----  --------  --------------------\n",
    );
    for (index, fit) in fits.iter().enumerate() {
        out.push_str(&format!(
            "{:<6}  {:<22}  {:<7}  | {:<10}  {:>4}  {:>5}  {:>8}  {}\n",
            fit.label,
            extent(&fit.fitted),
            route_word(fit.fitted.route),
            "page",
            "-",
            "-",
            residues[0][index],
            "-",
        ));
        for (column, (engine, shots)) in engines.iter().enumerate() {
            let Some(shot) = shots[index].as_ref() else {
                continue;
            };
            let (tiles, quality) = match &shot.declined {
                Some(key) => ("-".to_string(), format!("declined ({key})")),
                None => (
                    shot.tiles.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
                    verdict_word(&quality::assess(page, &shot.mask, &shot.pixels, noise)),
                ),
            };
            out.push_str(&format!(
                "{:<6}  {:<22}  {:<7}  | {:<10}  {:>4}  {:>5}  {:>8}  {}\n",
                "",
                "",
                "",
                rung_word(*engine),
                shot.elapsed.as_millis(),
                tiles,
                residues[column + 1][index],
                quality,
            ));
        }
    }
    out
}

/* ------------------------------------------------------------------ */
/* Small things                                                        */
/* ------------------------------------------------------------------ */

/// A patch built by hand, for the comparison only. Its provenance is a
/// placeholder: nothing here is written to a manifest.
fn a_patch(index: usize, engine: Engine, mask: Mask, pixels: Raster) -> Patch {
    Patch {
        id: format!("compare-{index}-{}", engine.rung_key()),
        // A placeholder's lettering is its mask: nothing here is exported.
        ink: mask.clone(),
        mask,
        pixels,
        order: index as u32,
        visible: true,
        provenance: Provenance {
            engine,
            engine_version: env!("CARGO_PKG_VERSION").into(),
            model_sha256: None,
            execution_provider: "spike".into(),
            params_snapshot: serde_json::json!({ "rung": engine.rung_key() }),
            mask_sha256: String::new(),
            source_sha256: String::new(),
            cloud: None,
            created: 0,
        },
    }
}

fn extent(fitted: &fit::Fitted) -> String {
    let bounds = fitted.mask.bounds;
    format!("{}X{} AT {},{}", bounds.w, bounds.h, bounds.x, bounds.y)
}

fn route_word(route: fit::Route) -> &'static str {
    match route {
        fit::Route::Fill => "FILL",
        fit::Route::Inpaint => "INPAINT",
    }
}

fn rung_word(engine: Engine) -> &'static str {
    match engine {
        Engine::Fill => "0 fill",
        Engine::Lama => "2 lama",
        other => match other {
            Engine::Flux => "3a flux",
            _ => "cloud",
        },
    }
}

fn verdict_word(assessment: &quality::Assessment) -> String {
    match assessment.cause() {
        Some(cause) => format!("declined ({cause:?})"),
        None => match assessment.outcome {
            quality::Outcome::Unmeasured(why) => format!("unmeasured ({why:?})"),
            _ => "accepted".into(),
        },
    }
}

fn upper(text: &str) -> String {
    text.rsplit('.').next().unwrap_or(text).to_uppercase()
}

fn provider(selection: &cleaner_core::accel::Selection) -> String {
    format!("{:?}", selection.accelerator).to_lowercase()
}

fn timed<T, E>(work: impl FnOnce() -> Result<T, E>) -> Result<(T, Duration), E> {
    let started = Instant::now();
    work().map(|value| (value, started.elapsed()))
}

/* ------------------------------------------------------------------ */
/* Memory                                                              */
/* ------------------------------------------------------------------ */

/// Current resident size in bytes, from `ps` - the same instrument
/// `spikes/strip-memory` uses, so the two harnesses' numbers are comparable.
/// Not the peak: `/usr/bin/time -l` around the whole process is what gives
/// that, and the earlier table is read the same way.
fn rss() -> Option<u64> {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

fn sample(into: &mut Vec<(String, u64)>, phase: &str) {
    let bytes = rss().unwrap_or(0);
    println!("  RSS {:>7.0} MB - {phase}", bytes as f64 / MB);
    into.push((phase.to_string(), bytes));
}

/// The phase-by-phase table, with the delta beside each row.
///
/// The delta is the column the exercise exists for. An
/// ONNX Runtime arena is not returned when the session that grew it is dropped,
/// so a "dropped" row that does not fall is the expected reading and not a
/// failed measurement - it is what makes the peak claim about *sessions* rather
/// than about the process.
fn report(rss: &[(String, u64)]) {
    println!("\nRSS at each phase boundary");
    println!("{:<34}  {:>10}  {:>10}", "phase", "RSS", "delta");
    let mut previous = None;
    for (phase, bytes) in rss {
        let delta = match previous {
            Some(before) => {
                format!("{:+.0} MB", (*bytes as f64 - before as f64) / MB)
            }
            None => "-".to_string(),
        };
        println!("{:<34}  {:>7.0} MB  {:>10}", phase, *bytes as f64 / MB, delta);
        previous = Some(*bytes);
    }
    if let Some(peak) = rss.iter().map(|(_, b)| *b).max() {
        println!("high-water mark of these samples: {:.0} MB", peak as f64 / MB);
    }
}

const MB: f64 = 1024.0 * 1024.0;
