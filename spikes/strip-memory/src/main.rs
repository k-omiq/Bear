//! Peak RSS over a long chapter, through the rules that decide it.
//!
//! ```text
//! /usr/bin/time -l cargo run --release -p spike-strip-memory -- --pages 200
//! ```
//!
//! ## What this measures, and what it does not
//!
//! Phase 2's exit criterion is "a 200-page
//! **webtoon chapter** cleans end to end with peak RSS under 400 MB (rungs 0/1
//! only)". This harness feeds it a **synthetic** strip, and the number it
//! produces is therefore not that criterion.
//!
//! `fixtures/pages/` standing in for a licence-cleared corpus is forbidden,
//! and that rule is about **content**
//! judgements - flag rate, Latin regions, inpainter quality - which are
//! properties of what is drawn. Peak RSS is a property of buffer sizes and of
//! the order things are allocated in, so a synthetic strip is a defensible
//! input for *this* measurement and for nothing else in the phase. What a
//! synthetic page does change is how much **work** there is: fewer detected
//! boxes means fewer patches held, so the number below is a floor on the real
//! one rather than an estimate of it.
//!
//! Two things the criterion names that are outside this process entirely:
//!
//! - **the webview**, 120-180 MB of the own budget. This is a command-line
//!   binary and there is no WKWebView in it.
//! - **the Tauri command layer and the event channel**, which the run adds
//!   around the same pipeline.
//!
//! So this is the Rust core's half of the budget - the row marked "Rust core,
//! idle ~40 MB", the detector, the balloon detector, the script gate, and the
//! decode window and working buffers - and the honest way to read the result is
//! against those rows and not against the 400 MB.
//!
//! The peak itself is **not** measured from inside this process. `getrusage`
//! is not reachable without a libc dependency this workspace does not carry, and
//! sampling `ps` gives a sampled maximum rather than a peak. `/usr/bin/time -l`
//! reports `maximum resident set size` from the kernel, which is the real one.
//! What this binary prints is the *current* RSS at each phase boundary, read
//! from `ps`, which is enough to say where the peak came from.
//!
//! ## Phase 3's memory criterion, per rung
//!
//! Phase 3 asks for something this harness
//! could not previously answer:
//!
//! > peak RSS over a multi-region page, **per rung** … Criterion: **flat across
//! > regions**. A rung whose peak grows with region count fails, and that is a
//! > separate failure from being large: LaMa at 250 MB flat passes and a 5 GB
//! > engine that ratchets does not.
//!
//! Three things were missing and all three are flags now.
//!
//! **A page with enough regions for a ratchet to show.** `--page <file>` reads
//! a real fixture instead of drawing a synthetic strip, and repeats it
//! `--pages` times. `fixtures/pages/page-crowded.png` is the one this exists
//! for: twenty detected regions against one to four on every other fixture, so
//! four copies of it is eighty regions through one held session.
//!
//! **The ladder, open.** `--rung 2` and `--rung 3` run the model rungs, which
//! the earlier rungs-0/1 harness never did.
//!
//! **Attribution.** `--rung` does not *route*; it says which single rung every
//! region that needs a model is run on. That is what "per rung" requires and it
//! is deliberately not what [`src-tauri`'s run](../../src-tauri/src/run.rs)
//! does - the run tries rung 2 and escalates to rung 3 only on a decline, which
//! attributes a peak to whichever rungs happened to be involved. A measurement
//! of one rung has to run one rung.
//!
//! The RSS is sampled **after every region**, which makes the number a series
//! rather than a maximum: a rung that ratchets shows growth against region
//! index, and a rung that is merely large shows a step at the first region and
//! a flat line after it. The verdict printed at the end is that comparison and
//! the numbers it was made from are printed beside it, so a reader who
//! disagrees with the bound can apply their own.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use cleaner_core::accel::{Accelerator, Preference};
use cleaner_core::detect::{Detector, build_regions};
use cleaner_core::engines::{fill, lama};
use cleaner_core::fit::{self, EdgeMap};
use cleaner_core::gate::ScriptGate;
use cleaner_core::image::{BitDepth, ColorMode, Format, Raster, decode, encode};
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::patch::Engine;
use cleaner_core::project::buffers;
use cleaner_core::strip::{
    self, DetectedInSegment, EngineContext, Split, SplitKind, Strip, merge_global,
};

struct Options {
    pages: u32,
    width: u32,
    height: u32,
    dir: PathBuf,
    keep: bool,
    /// `--cpu` forces `CpuOnly`. Worth a flag because the accelerator is one of
    /// the two things the peak turned out to be about.
    preference: Preference,
    /// Bypass the script gate, which is the `Clean all detected text`
    /// setting. On a synthetic page the gate is right to hold every region
    /// back - the marks are not Japanese - and a harness that measured the
    /// pipeline only as far as the gate would measure the cheap half of it.
    clean_all: bool,
    /// The rung every region that needs a model is run on: 0 or 1 for the
    /// earlier rungs-0/1 harness, 2 for manga-LaMa, 3
    /// for MI-GAN. **Attribution, not routing** - see the module docs.
    rung: u8,
    /// A real page to repeat instead of drawing a synthetic strip. What makes
    /// the region count high enough for Phase 3's criterion to be a test.
    page: Option<PathBuf>,
}

/// One region's cost, sampled the moment its patch was written.
struct Sample {
    /// Which region of the run this was, counting from one across every page.
    index: usize,
    rss_bytes: u64,
    /// The rung that produced it. Rungs 0 and 1 appear here too, because a
    /// run at `--rung 2` still fills the regions the fit settled and their
    /// samples are what the model rung's are read against.
    engine: Engine,
}

fn main() -> Result<()> {
    let options = parse()?;
    match &options.page {
        Some(page) => println!(
            "chapter: {} copies of {} - rung {}",
            options.pages,
            page.display(),
            options.rung
        ),
        None => println!(
            "synthetic chapter: {} pages of {}×{} - {} strip rows, rung {}",
            options.pages,
            options.width,
            options.height,
            options.pages as u64 * options.height as u64,
            options.rung,
        ),
    }
    report("start");

    let pages = draw(&options)?;
    report("pages written");

    let runtime = cleaner_core::runtime::find(None)
        .ok()
        .or_else(fetched_runtime)
        .context("no ONNX Runtime - run scripts/fetch-runtime.sh")?;
    cleaner_core::runtime::load(&runtime)?;
    let models = models()?;
    // Sessions are held for the whole run and regions are never batched.
    // Three of them, opened once.
    let mut detector = Detector::open(&models.join("comictextdetector.onnx"), options.preference)?;
    report("detector session held");
    let mut balloons = cleaner_core::balloon::BalloonDetector::open(
        &models.join("comic-text-and-bubble-detector-detector-v4-s_int8.onnx"),
        options.preference,
    )?;
    report("balloon session held");
    let mut gate = ScriptGate::open(
        &models.join("image-script-identification-osd_lstm.onnx"),
        &models.join("image-script-identification-osd_labels.json"),
        options.preference,
    )?;
    report("three sessions held");
    println!("  provider: {:?}", detector.selection().accelerator);

    // Rule 1's coordinate system, from the headers alone. Nothing is decoded to
    // build it.
    let sizes: Vec<(u32, u32)> = pages
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path)?;
            let header = cleaner_core::image::png_header(&bytes).map_err(|e| anyhow!("{e}"))?;
            Ok((header.width, header.height))
        })
        .collect::<Result<_>>()?;
    let strip = Strip::of_sizes(&sizes);

    // Rules 1 to 3: one walk over the pages, one page of samples at a time.
    let survey_started = Instant::now();
    let survey = strip::survey(&strip, |placement| {
        let bytes = std::fs::read(&pages[placement]).ok()?;
        decode(&bytes).ok()
    });
    println!(
        "survey: {:.1} s - {} splits, {} segments, {} join anomalies",
        survey_started.elapsed().as_secs_f64(),
        survey.splits.len(),
        survey.segments.len(),
        survey.anomalies().count()
    );
    report("surveyed");

    let cache = options.dir.join("cache");
    std::fs::create_dir_all(&cache)?;
    let cuts: Vec<Split> = survey
        .segments
        .iter()
        .skip(1)
        .map(|s| Split { y: s.start, kind: SplitKind::Join })
        .collect();

    // The ladder, as far as this run was asked to open it. Opened here rather
    // than lazily on purpose: the session's own cost belongs *before* the first
    // region's sample, or the first region carries it and looks like a ratchet
    // that never repeats.
    let mut rung2 = None;
    match options.rung {
        2 => {
            // The run digests the weights before it builds the session
            // (`src-tauri/src/run.rs#open_inpainter`), and the digest costs a
            // read of the whole file. Done here too, in the same order, because
            // the budget row this harness produces has to be the one the
            // application actually pays.
            digest(&models.join("lama-manga.onnx"));
            report("rung 2 weights digested");
            rung2 = Some(lama::Inpainter::open(&models.join("lama-manga.onnx"), options.preference)?);
            let where_it_ran = rung2.as_ref().unwrap().selection().accelerator;
            report(&format!("rung 2 session held on {where_it_ran:?}"));
        }
        // Rung 3 was MI-GAN, and MI-GAN is gone from the ladder; there is no
        // session to hold and nothing for this harness to measure at 3.
        3 => return Err(anyhow!("rung 3 (MI-GAN) was removed from the ladder; measure --rung 2")),
        _ => {}
    }

    let clean_started = Instant::now();
    let mut cleaned = 0usize;
    let mut regions_seen = 0usize;
    let mut declined = 0usize;
    let mut without_a_rung = 0usize;
    let mut samples: Vec<Sample> = Vec::new();
    for (placement, path) in pages.iter().enumerate() {
        let bytes = std::fs::read(path)?;
        let page = decode(&bytes).map_err(|e| anyhow!("{e}"))?;
        let balloon_boxes = balloons.detect(&page)?;
        let noise = fit::page_noise_sigma(&page);
        let origin = strip.pages()[placement];

        let mut previous: Vec<DetectedInSegment> = Vec::new();
        for segment in survey.segments_of(&strip, placement) {
            let Some((top, bottom)) = strip::crop_rows(&strip, placement, &segment) else {
                continue;
            };
            let cropped;
            let crop: &Raster = if top == 0 && bottom == page.height {
                &page
            } else {
                cropped = page.rows(top, bottom);
                &cropped
            };

            let detection = detector.detect(crop)?;
            if placement < 2 {
                report(&format!("page {} segment {} detected", placement + 1, segment.index));
            }
            let regions = build_regions(detection.boxes.clone(), crop.width, crop.height);
            let scale = detection.segmentation.proxy_scale();
            let edges = EdgeMap::sobel(crop);

            let to_strip = |rect: Rect| {
                Rect::new(
                    rect.x + origin.x_offset,
                    rect.y + origin.y_offset + top as i64,
                    rect.w,
                    rect.h,
                )
            };
            let found: Vec<DetectedInSegment> = regions
                .iter()
                .map(|r| DetectedInSegment::new(segment.index, to_strip(r.masking), 1.0))
                .collect();
            let mut evidence = previous.clone();
            evidence.extend(found.iter().copied());
            let globals = merge_global(&evidence, &cuts, &survey.segments);

            for region in regions.iter() {
                regions_seen += 1;
                let in_strip = to_strip(region.masking);
                let owned = globals
                    .iter()
                    .find(|g| {
                        g.rect.x <= in_strip.x
                            && g.rect.y <= in_strip.y
                            && g.rect.right() >= in_strip.right()
                            && g.rect.bottom() >= in_strip.bottom()
                    })
                    .map(|g| g.owner == segment.index)
                    .unwrap_or(true);
                if !owned {
                    continue;
                }

                let on_page = Rect::new(
                    region.masking.x,
                    region.masking.y + top as i64,
                    region.masking.w,
                    region.masking.h,
                );
                let detected = cleaner_core::balloon::detected(on_page, &balloon_boxes);
                // The paper is read in the crop, the balloon boxes on the page.
                let inside = cleaner_core::balloon::in_bubble(
                    crop,
                    &detection.segmentation,
                    region.text_bounds(),
                    on_page,
                    &balloon_boxes,
                );
                let verdict = gate.judge(
                    crop,
                    &detection.segmentation,
                    region,
                    detected,
                    inside,
                    cleaner_core::gate::OutsideText::Review,
                )?;
                if !options.clean_all && !verdict.cleans() {
                    continue;
                }
                let seed =
                    fit::seed_mask(&detection.segmentation, region, crop.width, crop.height);
                if seed.is_empty() {
                    continue;
                }

                // Rule 4, and rule 6: one window is read, worked on, written to
                // the cache and dropped.
                let window = strip::decode_window(
                    &strip,
                    &survey.joins,
                    in_strip,
                    scale,
                    EngineContext::None,
                );
                let bounds = Rect::new(
                    (window.rect.x - origin.x_offset).max(0),
                    (window.rect.y - origin.y_offset - top as i64).max(0),
                    window.rect.w,
                    window.rect.h,
                );
                let fitted = fit::fit_within(crop, &seed, scale, noise, &edges, false, bounds);

                // **Attribution, not routing.** Every region the fit sent to
                // the ladder is run on the one rung this invocation is
                // measuring, so the series below belongs to that rung and to
                // nothing else.
                let engine = if fitted.route.needs_a_model() {
                    match options.rung {
                        2 => Engine::Lama,
                        // Rung 0 only, which is what was measured: a region
                        // that needs a model is counted and left.
                        _ => {
                            without_a_rung += 1;
                            continue;
                        }
                    }
                } else {
                    Engine::Fill
                };

                // The **first** run of each model rung, either side of the
                // inference. The finding was that one detector inference
                // costs about 1.2 GB of allocator arena the budget table had no
                // row for; the same question is owed of every rung that holds a
                // session, and this is where it is answered.
                let first_of_its_rung = matches!(engine, Engine::Lama)
                    && !samples.iter().any(|s| s.engine == engine);
                if first_of_its_rung {
                    report(&format!("before the first {} inference", engine.rung_key()));
                }

                let rendered: Option<(Mask, Raster)> = match engine {
                    Engine::Lama => {
                        match rung2.as_mut().expect("rung 2 was asked for").render(crop, &fitted) {
                            Ok(rendered) => Some((rendered.mask, rendered.pixels)),
                            // A rung refusing a region before it runs is a
                            // decision, not a fault, and it costs the
                            // measurement one sample rather than the run.
                            Err(lama::Error::Declined(_)) => None,
                            Err(lama::Error::Run(fault)) => return Err(anyhow!(fault)),
                        }
                    }
                    _ => Some((fitted.mask.clone(), fill::render(crop, &fitted))),
                };
                if first_of_its_rung {
                    report(&format!("after the first {} inference", engine.rung_key()));
                }
                let Some((mask, pixels)) = rendered else {
                    declined += 1;
                    continue;
                };

                buffers::write_atomic(
                    &cache.join(format!("p{placement:04}-r{cleaned}.mask")),
                    &buffers::encode_mask(&mask),
                )
                .map_err(|e| anyhow!("{e}"))?;
                buffers::write_atomic(
                    &cache.join(format!("p{placement:04}-r{cleaned}.patch")),
                    &buffers::encode_patch(&pixels),
                )
                .map_err(|e| anyhow!("{e}"))?;
                cleaned += 1;
                // Rule 6's moment: the patch is on disk and the buffers are
                // about to go. Sampling here rather than at the top of the next
                // region keeps the series comparable region to region.
                drop(pixels);
                samples.push(Sample { index: cleaned, rss_bytes: rss().unwrap_or(0), engine });
            }
            previous = found;
        }

        if placement < 4 || placement % 25 == 24 {
            report(&format!("page {}", placement + 1));
        }
    }

    println!(
        "clean: {:.1} s - {regions_seen} regions seen, {cleaned} patches in the cache, \
         {declined} declined by their rung, {without_a_rung} left for want of one",
        clean_started.elapsed().as_secs_f64()
    );
    report("done");
    per_rung(&samples, options.rung);
    println!(
        "\nthe peak is the kernel's, not this process's own: run under `/usr/bin/time -l`\n\
         and read `maximum resident set size` (bytes on macOS)."
    );

    if !options.keep {
        let _ = std::fs::remove_dir_all(&options.dir);
    } else {
        println!("kept {}", options.dir.display());
    }
    Ok(())
}

/// Read and digest a model file, and drop it - what the run does before it
/// builds a session, and a 207 MB read the allocator does not necessarily hand
/// back to the operating system.
fn digest(model: &Path) {
    if let Ok(bytes) = std::fs::read(model) {
        let _ = cleaner_core::ingest::sha256_hex(&bytes);
    }
}

/// Current resident size in bytes, from `ps`. Not the peak - see the module
/// docs.
fn rss() -> Option<u64> {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

fn report(phase: &str) {
    match rss() {
        Some(bytes) => println!("  RSS now {:.0} MB - {phase}", bytes as f64 / MB),
        None => println!("  RSS unavailable - {phase}"),
    }
}

const MB: f64 = 1024.0 * 1024.0;

/// What a region is allowed to leave behind, in bytes.
///
/// The budget table gives "Decode
/// window + working buffers ~30 MB", and that row is the whole of what rule 6
/// permits to be alive for one region. So a run whose RSS has grown by more
/// than one region's worth of it, after the first region has paid for the
/// arena, is a run that kept something - which is exactly the ratchet
/// Phase 3's criterion is written against.
/// The bound is this harness's, it is stated rather than tuned, and every
/// number it was applied to is printed beside the verdict.
const RATCHET_BOUND_BYTES: f64 = 30.0 * 1024.0 * 1024.0;

/// The fewest regions this can honestly judge.
///
/// The criterion is about growth *with region count*, so it needs enough
/// regions for a slope to be distinguishable from a step. Under this it says so
/// and reports nothing, which is what the old harness should have
/// been doing: "its input has few regions per page, which is the axis the
/// criterion is written against".
const ENOUGH_REGIONS: usize = 8;

/// Phase 3's memory exit criterion, answered.
///
/// > peak RSS over a multi-region page, per rung … Criterion: **flat across
/// > regions**. A rung whose peak grows with region count fails, and that is a
/// > separate failure from being large.
///
/// **The series has a step in it before it has a slope, and the two are not the
/// same finding.** The earlier measurement found the ONNX Runtime allocator arena reaching its
/// high-water mark on the first inference and never returning it - "the first
/// page pays for every page" - and a second, smaller high-water arrives with
/// the first segment shape that differs from the first one's. Both are steps:
/// they happen once, they do not repeat, and a run of eight pages sits at
/// exactly the same RSS as a run of four. Reading either as growth would report
/// every rung in this application as ratcheting, which is the failure the
/// criterion's own wording rules out - being large is not being unbounded.
///
/// So the series is judged in halves. The first half carries the steps and is
/// **reported**; the second half is where a ratchet has to show, and it is what
/// the verdict is taken on: the peak over the second half against the RSS at
/// the moment the second half began. A rung that keeps anything per region
/// grows there in proportion to the regions in the window, and this harness's
/// windows are forty to eighty regions wide.
fn per_rung(samples: &[Sample], rung: u8) {
    println!("\nPhase 3's memory criterion - peak RSS per rung, flat across regions");
    if samples.is_empty() {
        println!("  no regions were cleaned, so there is nothing to report - not a pass");
        return;
    }
    for engine in [Engine::Fill, Engine::Lama] {
        let series: Vec<&Sample> = samples.iter().filter(|s| s.engine == engine).collect();
        if series.is_empty() {
            continue;
        }
        let name = engine.rung_key();
        let at = |index: usize| series[index].rss_bytes as f64;
        let peak = series.iter().map(|s| s.rss_bytes).max().unwrap() as f64;
        println!(
            "  {name}: {} regions - first {:.0} MB, last {:.0} MB, peak {:.0} MB",
            series.len(),
            at(0) / MB,
            at(series.len() - 1) / MB,
            peak / MB,
        );
        if series.len() < ENOUGH_REGIONS {
            println!(
                "    {} regions is too few to tell a step from a slope - unmeasured, not met",
                series.len()
            );
            continue;
        }
        let half = series.len() / 2;
        let step = at(half) - at(0);
        let settled = at(half);
        let peak_after = series[half..].iter().map(|s| s.rss_bytes).max().unwrap() as f64;
        let growth = peak_after - settled;
        let each = growth / (series.len() - half - 1).max(1) as f64;
        println!(
            "    first half: {:+.0} MB - the allocator arenas, which are steps and do not repeat",
            step / MB
        );
        println!(
            "    second half, regions {}–{}: settled at {:.0} MB, peak {:.0} MB, growth {:+.1} MB \
             ({:+.2} MB per region)",
            series[half].index,
            series[series.len() - 1].index,
            settled / MB,
            peak_after / MB,
            growth / MB,
            each / MB,
        );
        if growth <= RATCHET_BOUND_BYTES {
            println!(
                "    **MET** - flat across regions, inside the {:.0} MB one region is allowed to \
                 have alive",
                RATCHET_BOUND_BYTES / MB
            );
        } else {
            println!(
                "    **NOT MET** - the peak grows with region count, by {:.0} MB against a \
                 {:.0} MB bound",
                growth / MB,
                RATCHET_BOUND_BYTES / MB
            );
        }
    }
    println!(
        "  measured at --rung {rung}: every region that needed a model ran on that rung, so the \
         series above is one rung's and not a route's."
    );
}

/// A synthetic chapter: flat paper, panel rules, and a few filled rounded
/// rectangles standing in for balloons with text in them.
///
/// Deliberately dull. What it has to be is the right **size**, in the right
/// number of files, with enough structure that rule 2's row profile is not
/// uniformly zero - nothing about how it looks is evidence for anything.
fn draw(options: &Options) -> Result<Vec<PathBuf>> {
    // A real page, repeated. Nothing is copied and nothing is drawn: the
    // criterion is about region *count* through one held session, and the same
    // file listed `--pages` times is the cheapest honest way to get one.
    if let Some(page) = &options.page {
        if !page.exists() {
            return Err(anyhow!("no such page: {}", page.display()));
        }
        return Ok(vec![page.clone(); options.pages as usize]);
    }
    std::fs::create_dir_all(&options.dir)?;
    let (w, h) = (options.width, options.height);
    let mut paths = Vec::with_capacity(options.pages as usize);

    for n in 0..options.pages {
        let path = options.dir.join(format!("{:04}.png", n + 1));
        if !path.exists() {
            let mut data = vec![246u8; (w * h) as usize];
            let put = |data: &mut Vec<u8>, x: u32, y: u32, v: u8| {
                if x < w && y < h {
                    data[(y * w + x) as usize] = v;
                }
            };
            // Panel rules, so the profile has loud rows.
            for band in 0..(h / 400).max(1) {
                let y = band * 400 + 20;
                for x in 30..w - 30 {
                    put(&mut data, x, y, 30);
                }
            }
            // Balloons: a light interior, a dark outline, and rows of glyph-ish
            // marks inside.
            for balloon in 0..(h / 400).max(1) {
                let (bx, by) = (120 + (balloon % 3) * 180, balloon * 400 + 90);
                let (bw, bh) = (220u32, 180u32);
                for y in by..(by + bh).min(h) {
                    for x in bx..(bx + bw).min(w) {
                        let edge = x == bx || x + 1 == bx + bw || y == by || y + 1 == by + bh;
                        put(&mut data, x, y, if edge { 20 } else { 252 });
                    }
                }
                for row in 0..5u32 {
                    let y = by + 24 + row * 28;
                    for x in (bx + 24)..(bx + bw - 24) {
                        if (x / 4 + row) % 3 != 0 {
                            put(&mut data, x, y, 25);
                            put(&mut data, x, y + 1, 25);
                        }
                    }
                }
            }
            let raster = Raster {
                width: w,
                height: h,
                mode: ColorMode::Gray,
                depth: BitDepth::Eight,
                icc: None,
                palette: None,
                trns: None,
                srgb_intent: None,
                data,
            };
            std::fs::write(&path, encode(&raster, Format::Png).map_err(|e| anyhow!("{e}"))?)?;
        }
        paths.push(path);
    }
    Ok(paths)
}

/// The checkout's own `runtimes/`, which is where `scripts/fetch-runtime.sh`
/// unpacks. `runtime::find` looks where an *installed* application would, and a
/// spike binary in `target/release` is not one.
fn fetched_runtime() -> Option<PathBuf> {
    let name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    for root in ["runtimes", "../runtimes", "../../runtimes"] {
        if let Ok(entries) = std::fs::read_dir(root) {
            if let Some(path) = entries
                .flatten()
                .map(|entry| entry.path().join("lib").join(name))
                .find(|path| path.exists())
            {
                return Some(path);
            }
        }
    }
    None
}

fn models() -> Result<PathBuf> {
    for candidate in ["models", "../models", "../../models"] {
        let dir = Path::new(candidate);
        if dir.join("comictextdetector.onnx").exists() {
            return Ok(dir.to_path_buf());
        }
    }
    Err(anyhow!("no model weights - run scripts/fetch-models.sh"))
}

fn parse() -> Result<Options> {
    let mut options = Options {
        pages: 200,
        width: 800,
        height: 1280,
        dir: std::env::temp_dir().join("manga-cleaner-strip-memory"),
        keep: false,
        preference: Preference::Automatic,
        clean_all: true,
        rung: 1,
        page: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--pages" => options.pages = value()?.parse()?,
            "--width" => options.width = value()?.parse()?,
            "--height" => options.height = value()?.parse()?,
            "--dir" => options.dir = PathBuf::from(value()?),
            "--keep" => options.keep = true,
            "--cpu" => options.preference = Preference::CpuOnly,
            "--gate" => options.clean_all = false,
            "--rung" => options.rung = value()?.parse()?,
            "--page" => options.page = Some(PathBuf::from(value()?)),
            "--provider" => {
                let name = value()?;
                options.preference = Preference::Force(match name.as_str() {
                    "cpu" => Accelerator::Cpu,
                    "coreml" => Accelerator::CoreMl,
                    "webgpu" => Accelerator::WebGpu,
                    other => return Err(anyhow!("unknown provider {other}")),
                });
            }
            other => return Err(anyhow!("unknown flag {other}")),
        }
    }
    Ok(options)
}
