//! Clean one page, end to end - and produce something a person can look at.
//!
//! ```text
//! spike-clean-page <page.png> [--out-dir DIR]
//! spike-clean-page <page.png> --compare-engines [--out-dir DIR]
//! ```
//!
//! Two modes, and they do not run together. The default cleans the page through
//! the ladder; `--compare-engines` runs [`compare`], which puts all four rungs
//! that produce pixels side by side on the same regions.
//!
//! **They are exclusive on purpose.** [`Pipeline`] holds three detection
//! sessions for the length of a run and opens rung 2's and rung 3's beneath
//! them, and [`compare`]'s whole subject is what the process weighs with exactly
//! one session alive. A comparison run inside a live `Pipeline` would report the
//! sum of two harnesses, which is the mistake the mode was written to stop
//! making. So `--compare-engines` returns before the ladder run begins.
//!
//! Three things come out of a default run, and they answer three different
//! questions.
//!
//! **The cleaned page**, exported through the color fidelity and export
//! contract's own path and then read back from disk, with the fidelity contract
//! asserted against the file that was actually written. That is the question
//! *did the edit stay inside the mask*, and it is the one this spike has always
//! answered.
//!
//! **A before/after pair and a rung overlay**, which answer the question no
//! assertion in this repository asks: *does the result look right*. The overlay
//! marks every region with the rung that cleaned it, so a fill that should have
//! been an inpaint and an inpaint that should have been left alone are visible
//! rather than inferred from a table of routes.
//!
//! **Every engine that produces pixels, side by side** (`--compare-engines`),
//! which is [`compare`]'s module documentation and not this one's. It is a
//! plausibility check and **not** a quality comparison: that wants a
//! licence-cleared corpus, and synthetic fixtures are forbidden from
//! standing in for one.
//!
//! ## Why this calls the adapter
//!
//! The escalation loop - try the routed rung, score it with
//! [`cleaner_core::quality`], escalate or decline - lives in
//! `src-tauri/src/run.rs`, and there is exactly one of it. An earlier version of
//! this file refused to duplicate it and therefore skipped every region routed
//! to the ladder, which meant the spike could not show a rung-2 fill at all.
//! Duplicating it here would have been worse: two ladders that agree today and
//! drift the first time one of them learns a rung.
//!
//! So this depends on `manga-cleaner` and drives [`Pipeline`], which **is** the
//! ladder, through the [`Cleaner`] trait the scheduler uses. What the spike
//! still owns is everything either side of it: the routing report, the images,
//! and the contract asserted against the file on disk.
//!
//! Rung 3 (MI-GAN) is gone from the ladder, so [`compare`] shows rungs 0 to 2
//! and nothing else; `ladder_from` names the same three.

mod compare;
mod paint;

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use cleaner_core::accel::Preference;
use cleaner_core::composite::{changed_pixels, permitted_region};
use cleaner_core::export::{Target, export_page};
use cleaner_core::image::{Format, Raster, decode, encode};
use cleaner_core::patch::{Engine, Patch};
use cleaner_core::strip::{self, Strip};
use manga_cleaner_lib::run::{
    Cleaner, HIGHEST_LOCAL, PageContext, PageOutcome, Pipeline, RegionOutcome, model_search_paths,
};

use paint::{beside, outline, rgb8};

/* ------------------------------------------------------------------ */
/* The run                                                             */
/* ------------------------------------------------------------------ */

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let page_path = PathBuf::from(args.next().ok_or_else(|| {
        anyhow!("usage: spike-clean-page <page.png> [--out-dir DIR] [--compare-engines]")
    })?);
    let mut out_dir = PathBuf::from("target/spike-clean-page");
    let mut comparing = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--out-dir" => out_dir = PathBuf::from(args.next().context("--out-dir needs a path")?),
            "--compare-engines" => comparing = true,
            other => return Err(anyhow!("unknown argument {other}")),
        }
    }
    std::fs::create_dir_all(&out_dir)?;
    let stem = page_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page".into());

    cleaner_core::runtime::load(&cleaner_core::runtime::find(None).context("ONNX Runtime")?)?;
    let models = models_dir().context("no model weights - run scripts/fetch-models.sh")?;
    println!("models: {}", models.display());

    let source_bytes = std::fs::read(&page_path)?;
    let page = decode(&source_bytes).map_err(|e| anyhow!("{e}"))?;
    println!(
        "page: {} - {}×{} {:?} {:?}",
        page_path.display(),
        page.width,
        page.height,
        page.mode,
        page.depth
    );

    // Before any `Pipeline`, and returning rather than falling through, for the
    // reason the module documentation gives: the comparison's subject is what
    // the process weighs with one session alive.
    if comparing {
        return compare::run(&models, &page, &out_dir, &stem);
    }

    /* -------------------------------------------------------------- */
    /* The ladder, as the scheduler runs it                            */
    /* -------------------------------------------------------------- */

    let opened = std::time::Instant::now();
    let mut pipeline =
        Pipeline::open(&models, Preference::Automatic).map_err(|e| anyhow!("{e}"))?;
    println!("three sessions opened in {:.2} s", opened.elapsed().as_secs_f64());

    // A single-page project is the degenerate case of rules 1 to 4: one
    // page, its own strip, one segment, no joins.
    let strip = Strip::of_sizes(&[(page.width, page.height)]);
    let survey = strip::survey(&strip, |_| None);
    let context = PageContext {
        strip: &strip,
        joins: &survey.joins,
        segments: &survey.segments,
        placement: 0,
        sources: &[],
    };

    let whole = std::time::Instant::now();
    let outcome: PageOutcome = pipeline
        .clean_page("spike-p001", &source_bytes, HIGHEST_LOCAL, &context)
        .map_err(|e| anyhow!("{e}"))?;
    let elapsed = whole.elapsed();

    let mut patches: Vec<Patch> = Vec::new();
    let mut per_rung = [0u32; 2];
    let mut untouched = 0u32;
    for region in &outcome.regions {
        println!("  {}", one_line(region));
        match region {
            RegionOutcome::Cleaned(patch, _) => {
                match patch.provenance.engine {
                    Engine::Fill => per_rung[0] += 1,
                    _ => per_rung[1] += 1,
                }
                patches.push((**patch).clone());
            }
            RegionOutcome::Untouched { .. } | RegionOutcome::Candidate { .. } | RegionOutcome::Declined { .. } => untouched += 1,
            RegionOutcome::Detected(_) => return Err(anyhow!("auto clean returned a detect-only region")),
        }
    }
    println!(
        "\n{} regions: {} at rung 0, {} at rung 2, {} left alone - {:.2} s",
        outcome.regions.len(),
        per_rung[0],
        per_rung[1],
        untouched,
        elapsed.as_secs_f64()
    );
    if let Some(patch) = patches.iter().find(|p| p.provenance.engine == Engine::Lama) {
        println!(
            "rung 2 ran on {} (weights digested: {})",
            patch.provenance.execution_provider,
            patch.provenance.model_sha256.is_some()
        );
    }

    /* -------------------------------------------------------------- */
    /* What came out, as files                                         */
    /* -------------------------------------------------------------- */

    let export = export_page(&source_bytes, &patches, Target::SameAsSource)?;
    let cleaned_path = out_dir.join(format!("{stem}.cleaned.png"));
    std::fs::write(&cleaned_path, &export.bytes)?;
    println!("wrote {} ({} bytes)", cleaned_path.display(), export.bytes.len());

    // The contract, against the file on disk rather than against an in-memory
    // buffer that never went through an encoder.
    let out = decode(&export.bytes).map_err(|e| anyhow!("{e}"))?;
    let mode_ok = out.mode == page.mode && out.depth == page.depth && out.icc == page.icc;
    let changed = changed_pixels(&page, &out);
    let permitted = permitted_region(&patches, page.width, page.height);
    let outside = changed.iter().filter(|(x, y)| !permitted.contains(*x as i64, *y as i64)).count();
    println!("mode/depth/icc preserved: {mode_ok}");
    println!(
        "changed {} px, {} of them outside dilate(union(masks), edit_margin)",
        changed.len(),
        outside
    );

    let before = rgb8(&page);
    let after = rgb8(&out);
    let pair = beside(&[&before, &after]);
    let pair_path = out_dir.join(format!("{stem}.before-after.png"));
    std::fs::write(&pair_path, encode(&pair, Format::Png).map_err(|e| anyhow!("{e}"))?)?;
    println!("wrote {}", pair_path.display());

    let overlay_path = out_dir.join(format!("{stem}.overlay.png"));
    let overlay = rung_overlay(&after, &outcome);
    std::fs::write(&overlay_path, encode(&overlay, Format::Png).map_err(|e| anyhow!("{e}"))?)?;
    println!(
        "wrote {} - blue rung 0, green rung 1, red rung 2, grey left alone",
        overlay_path.display()
    );

    if !mode_ok || outside > 0 {
        return Err(anyhow!("the fidelity contract was violated"));
    }
    Ok(())
}

/// Where the weights are. [`model_search_paths`] is the adapter's own list; the
/// extra entry is the one a spike run from inside `spikes/` needs.
fn models_dir() -> Option<PathBuf> {
    model_search_paths(None)
        .into_iter()
        .chain(std::iter::once(PathBuf::from("../models")))
        .find(|dir| dir.join("comictextdetector.onnx").exists())
}

/// What one region came out as, in one line - the same evidence
/// `src-tauri/src/run/tests.rs` prints, because a spike and a test disagreeing
/// about how to describe a region is a difference somebody has to reconcile by
/// hand.
fn one_line(region: &RegionOutcome) -> String {
    match region {
        RegionOutcome::Cleaned(patch, review) => {
            let provenance = &patch.provenance;
            let params = &provenance.params_snapshot;
            format!(
                "{}: {:?} on {} - ({}, {}) {}×{} mask, {} ms, tiles {}, pad {}, quality {}{}",
                patch.id,
                provenance.engine,
                provenance.execution_provider,
                patch.mask.bounds.x,
                patch.mask.bounds.y,
                patch.mask.bounds.w,
                patch.mask.bounds.h,
                params["elapsed_ms"],
                params.get("tiles").map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
                params["edge_pad"],
                params["quality"],
                review.as_deref().map(|r| format!(" - {r}")).unwrap_or_default(),
            )
        }
        RegionOutcome::Untouched { bbox, reason } | RegionOutcome::Declined { bbox, reason, .. } => {
            format!("({}, {}) {}×{}: untouched - {reason}", bbox.x, bbox.y, bbox.w, bbox.h)
        }
        RegionOutcome::Candidate { bbox, reason, inside_bubble, .. } => {
            format!("({}, {}) {}×{}: candidate - {reason}, in a balloon: {inside_bubble}", bbox.x, bbox.y, bbox.w, bbox.h)
        }
        RegionOutcome::Detected(found) => {
            let bbox = found.record.bbox;
            format!("({}, {}) {}×{}: detected", bbox.x, bbox.y, bbox.w, bbox.h)
        }
    }
}

/// The cleaned page with every region outlined in the colour of the rung that
/// cleaned it.
fn rung_overlay(cleaned: &Raster, outcome: &PageOutcome) -> Raster {
    const RUNG0: [u16; 3] = [40, 90, 230];
    const RUNG2: [u16; 3] = [225, 45, 45];
    const LEFT_ALONE: [u16; 3] = [140, 140, 140];

    let mut canvas = cleaned.clone();
    for region in &outcome.regions {
        let (rect, colour) = match region {
            RegionOutcome::Cleaned(patch, _) => (
                patch.mask.bounds,
                match patch.provenance.engine {
                    Engine::Fill => RUNG0,
                    _ => RUNG2,
                },
            ),
            RegionOutcome::Untouched { bbox, .. } | RegionOutcome::Candidate { bbox, .. } | RegionOutcome::Declined { bbox, .. } => (*bbox, LEFT_ALONE),
            RegionOutcome::Detected(found) => (found.record.bbox, LEFT_ALONE),
        };
        outline(&mut canvas, rect, colour);
    }
    canvas
}
