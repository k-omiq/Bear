//! Debug-only stage capture for the cloud mask contract.
//!
//! For every cached cloud result of a **copied** chapter this rebuilds the
//! request the way the app prepared it, checks the rebuild against the
//! attempt journal's crop and hint digests, re-composites the cached raw
//! return, compares that with the stored patch, and writes one PNG per stage
//! at the crop's own geometry:
//!
//! `source` (page pixels, no predecessors), `input` (the uploaded crop),
//! `sam` and `group` (raw SAM-TS-L lettering and its text group, when a SAM
//! cache for the page is given), `det_ink` / `det_mask` (the stored
//! detection's lettering-plus-margin and fitted masks), `hint`, `alpha` (the
//! composite weight), `raw` (the model's return), `composite` (the recomputed
//! blend), `stored` (the patch as saved) and `aligned` (the same raw return
//! composited with the current preprocessing's tone alignment, whose report
//! goes into `meta.json`). `v2_input` and `v2_alpha` are the crop and weight
//! the current preprocessing would send and write for the same record.
//! `meta.json` names the exact models behind `sam` and `group` (checkpoint,
//! pinned revision, where it ran, spatial input) and the render behind `raw`
//! (recipe, model, revision, preprocessing version).
//!
//! **It reads its inputs and nothing else, and writes only `MC_STAGES_OUT`.**
//! Every path it reads is resolved (symlinks, `..`, relative paths) and
//! refused inside the app's live data folder, where every chapter and attempt
//! journal lives ([`live_roots`]). A chapter is opened read-only
//! ([`Job::open_read_only`]: no lock file, no maintenance), every file its
//! manifest names must resolve inside its own folders, and a region id from a
//! journal names an output folder only when it is a plain file name.
//! `MC_STAGES_OUT` must be a new or empty folder, and every file written in
//! it is created new, never through a link ([`Output`]).
//!
//! ```text
//! MC_STAGES_JOB=<copy>/c32.mtclean MC_STAGES_SNAPSHOT=<copy>/detect-snapshot/c32.mtclean \
//! MC_STAGES_ATTEMPTS=<copy>/cloud_attempts/attempts MC_STAGES_OUT=<scratch>/stages \
//! [MC_STAGES_SAM=<copy of the grouping measurement cache>] [MC_STAGES_REGIONS=c32-p004-d10,...] \
//! cargo run -p manga-cleaner --example mask_stages
//! ```

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use cleaner_core::balloon::{BalloonBox, BalloonClass};
use cleaner_core::engines::render::{GeneratedCrop, Preprocessing};
use cleaner_core::image::{BitDepth, ColorMode, Format, Raster};
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::project::Job;
use cleaner_core::text_groups::{self, EvidenceModel, Execution, Inputs, ModelUse, SpatialInput};
use serde_json::{json, Value};

/// What one capture reads and where it writes.
pub struct Config {
    /// The copied chapter's manifest.
    pub job: PathBuf,
    /// The copied attempt journals: one folder per attempt, each with its
    /// `journal.json` and cached `result.png`.
    pub attempts: PathBuf,
    /// Where every stage is written, and the only place anything is.
    pub out: PathBuf,
    /// The chapter as Detect left it, before any patch.
    pub snapshot: Option<PathBuf>,
    pub sam: Option<PathBuf>,
    pub regions: Option<Vec<String>>,
    /// Folders nothing is read from or written to ([`live_roots`]).
    pub live: Vec<PathBuf>,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name).ok().filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The app's data folder on this machine: `app_data_dir` for the bundle
/// identifier in `tauri.conf.json`, which holds the library of chapters
/// (`library/`) and the attempt journals (`cloud_attempts/`).
pub fn live_roots() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/com.mangacleaner.studio"))
        .into_iter()
        .collect()
}

/// Whether `path` is `root` or inside it. Both are resolved paths; the
/// comparison ignores case where the file system does.
fn within(path: &Path, root: &Path) -> bool {
    if path.starts_with(root) {
        return true;
    }
    if cfg!(target_os = "macos") {
        let lower = |p: &Path| p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect::<Vec<_>>();
        let (path, root) = (lower(path), lower(root));
        return path.len() >= root.len() && path[..root.len()] == root[..];
    }
    false
}

/// The resolved live folders, and the reads and writes checked against them.
struct Guard {
    live: Vec<PathBuf>,
}

impl Guard {
    fn new(live: &[PathBuf]) -> Guard {
        // A live folder that does not exist yet cannot be aliased; one that
        // does is compared resolved, as every path checked against it is.
        let live = live.iter().flat_map(|root| [root.clone(), std::fs::canonicalize(root).unwrap_or_else(|_| root.clone())]).collect();
        Guard { live }
    }

    /// `path`, resolved, refused inside a live folder.
    ///
    /// A hard link into the live data has a path of its own, outside it, and
    /// passes: no path check can tell it from a copy. It is only ever read
    /// (the chapter through [`Job::open_read_only`], everything else through
    /// [`Guard::read`]), and nothing is written anywhere but a fresh output
    /// ([`Output`]), so a linked live file is left as it was.
    fn resolve(&self, path: &Path) -> Result<PathBuf, String> {
        let resolved = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(root) = self.live.iter().find(|root| within(&resolved, root)) {
            return Err(format!(
                "{} resolves to {}, inside the live application data {}; copy it first",
                path.display(), resolved.display(), root.display()
            ));
        }
        Ok(resolved)
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, String> {
        let resolved = self.resolve(path)?;
        std::fs::read(&resolved).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// [`Guard::read`] for a file that may not be there.
    fn read_if_present(&self, path: &Path) -> Result<Option<Vec<u8>>, String> {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            _ => self.read(path).map(Some),
        }
    }

    /// Every link under `dir` (not followed while walking) leads outside the
    /// live folders.
    fn check_tree(&self, dir: &Path) -> Result<(), String> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let kind = entry.file_type().map_err(|e| format!("{}: {e}", entry.path().display()))?;
            if kind.is_symlink() {
                // A link to nothing leads nowhere a read could reach.
                if std::fs::metadata(entry.path()).is_ok() {
                    self.resolve(&entry.path())?;
                }
            } else if kind.is_dir() {
                self.check_tree(&entry.path())?;
            }
        }
        Ok(())
    }

    /// Everything reading `job` through the store can open: its sources, and
    /// its sidecar folder, whose files every reference the manifest makes
    /// must name by a plain relative path.
    fn check_job(&self, job: &Job) -> Result<(), String> {
        let sidecar = job.sidecar();
        if sidecar.exists() {
            self.resolve(sidecar)?;
            self.check_tree(sidecar)?;
        }
        let project = serde_json::to_value(&job.project).map_err(|e| e.to_string())?;
        let mut references = Vec::new();
        collect_references(&project, &mut references);
        let patches = job.project.patches.iter()
            .chain(&job.project.text_shape_patch_revisions)
            .chain(&job.project.legacy_patch_revisions);
        references.extend(patches.map(|record| record.ink_ref()));
        references.extend(job.project.detections.iter().map(|record| record.ink_ref()));
        if let Some(bad) = references.iter().find(|reference| !plain_relative(reference)) {
            return Err(format!("{} names {bad:?}, outside its own folder", job.path().display()));
        }
        for index in 0..job.project.sources.len() {
            if let Some(source) = job.source_path(index).filter(|path| path.exists()) {
                self.resolve(&source)?;
            }
        }
        Ok(())
    }
}

/// Every string a manifest keeps under a `*_ref` key: the files its store
/// resolves against the sidecar folder.
fn collect_references(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                match value {
                    Value::String(reference) if key.ends_with("_ref") => out.push(reference.clone()),
                    other => collect_references(other, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_references(item, out)),
        _ => {}
    }
}

/// A relative path of ordinary names only: no root, no `..`, no `.`.
fn plain_relative(reference: &str) -> bool {
    let path = Path::new(reference);
    !reference.is_empty() && path.components().all(|component| matches!(component, Component::Normal(_)))
}

/// A region id is used as a folder name only when it is a plain one.
fn safe_name(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id != "."
        && id != ".."
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Where the stages are written: a folder this capture made, or found empty,
/// resolved, outside every input and the live data. Every folder and file in
/// it is created new (`O_EXCL`), and a file is opened without following a
/// link (`O_NOFOLLOW`), so nothing that was there before, or was put there
/// since, is ever written through: not a link, not a hard link to a live
/// manifest. That is also what closes the gap between checking a path and
/// opening it: there is no existing file to swap in.
struct Output {
    root: PathBuf,
}

impl Output {
    fn new(guard: &Guard, out: &Path, inputs: &[PathBuf]) -> Result<Output, String> {
        let allowed = |resolved: &Path| match inputs.iter().find(|input| within(resolved, input)) {
            Some(input) => Err(format!("{} is inside the input {}; write somewhere else", out.display(), input.display())),
            None => Ok(()),
        };
        // Checked before a folder is made: the nearest part of it that
        // exists already, resolved, is where the new folders would go.
        let absolute = std::path::absolute(out).map_err(|e| format!("{}: {e}", out.display()))?;
        let existing = absolute.ancestors().find(|ancestor| ancestor.exists()).ok_or("no part of the output exists")?;
        allowed(&guard.resolve(existing)?)?;
        match std::fs::read_dir(out) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(format!("{} exists and is not empty; name a new folder", out.display()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
            }
            Err(e) => return Err(format!("{}: {e}", out.display())),
        }
        let root = guard.resolve(out)?;
        allowed(&root)?;
        Ok(Output { root })
    }

    /// The folder `name` (checked plain), made new inside the output.
    fn stage(&self, name: &str) -> Result<PathBuf, String> {
        if !safe_name(name) {
            return Err(format!("{name:?} is not a plain folder name"));
        }
        let dir = self.root.join(name);
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(dir)
    }

    /// Write `bytes` as a new file `name` in `dir`, one of this output's
    /// folders: refused if anything is at that name already, a link included.
    fn write(&self, dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
        use std::io::Write as _;
        if !(dir == self.root || dir.parent() == Some(self.root.as_path())) || !safe_name(name) {
            return Err(format!("{} is not inside {}", dir.join(name).display(), self.root.display()));
        }
        let path = dir.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        file.write_all(bytes).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn png(&self, dir: &Path, name: &str, raster: &Raster) -> Result<(), String> {
        let bytes = cleaner_core::image::encode(raster, Format::Png).map_err(|e| e.to_string())?;
        self.write(dir, &format!("{name}.png"), &bytes)
    }
}

fn rgb(width: u32, height: u32, data: Vec<u8>) -> Raster {
    Raster {
        width, height, mode: ColorMode::Rgb, depth: BitDepth::Eight,
        icc: None, palette: None, trns: None, srgb_intent: Some(1), color: Default::default(), data,
    }
}

fn gray(width: u32, height: u32, data: Vec<u8>) -> Raster {
    Raster { mode: ColorMode::Gray, ..rgb(width, height, data) }
}

/// `page` over `rect` (page coordinates) as RGB8, edge-replicated.
fn page_rgb(page: &Raster, rect: Rect) -> Result<Raster, String> {
    let mut data = Vec::with_capacity(rect.w as usize * rect.h as usize * 3);
    let managed = cleaner_core::image::color::ManagedColor::for_raster(page).map_err(|e|e.to_string())?;
    for y in 0..rect.h as i64 {
        for x in 0..rect.w as i64 {
            let px = (rect.x + x).clamp(0, page.width as i64 - 1) as u32;
            let py = (rect.y + y).clamp(0, page.height as i64 - 1) as u32;
            let shown = managed.pixel(page,px,py);
            for value in shown { data.push((value*255.0).round().clamp(0.0,255.0) as u8); }
        }
    }
    Ok(rgb(rect.w, rect.h, data))
}

/// `mask` over `rect` as an 8-bit image.
fn mask_gray(mask: &Mask, rect: Rect) -> Raster {
    let mut data = vec![0u8; rect.w as usize * rect.h as usize];
    for y in 0..rect.h as i64 {
        for x in 0..rect.w as i64 {
            if mask.contains(rect.x + x, rect.y + y) {
                data[(y * rect.w as i64 + x) as usize] = 255;
            }
        }
    }
    gray(rect.w, rect.h, data)
}

fn shifted(mut mask: Mask, dx: i64, dy: i64) -> Mask {
    mask.bounds.x += dx;
    mask.bounds.y += dy;
    mask
}

/// Paint `pixels` (page mode, placed at `at` in window coordinates) over an
/// RGB8 crop whose origin is `crop` in the same coordinates.
fn paint(onto: &mut Raster, crop: Rect, pixels: &Raster, at: Rect) -> Result<(), String> {
    let managed = cleaner_core::image::color::ManagedColor::for_raster(pixels).map_err(|e|e.to_string())?;
    for y in 0..at.h as i64 {
        for x in 0..at.w as i64 {
            let (cx, cy) = (at.x + x - crop.x, at.y + y - crop.y);
            if cx < 0 || cy < 0 || cx >= crop.w as i64 || cy >= crop.h as i64 {
                continue;
            }
            let shown=managed.pixel(pixels,x as u32,y as u32);
            for (c, value) in shown.iter().enumerate() {
                onto.data[(cy as usize * crop.w as usize + cx as usize) * 3 + c] = (value*255.0).round().clamp(0.0,255.0) as u8;
            }
        }
    }
    Ok(())
}

struct PageSam {
    width: u32,
    tiled: Vec<u8>,
    grouping: text_groups::Grouping,
}

fn load_sam(guard: &Guard, dir: &Path, stem: &str, width: u32, height: u32) -> Result<Option<PageSam>, String> {
    let (Some(tiled), Some(boxes)) = (
        guard.read_if_present(&dir.join(format!("{stem}.tiled.bin")))?,
        guard.read_if_present(&dir.join(format!("{stem}.rt.json")))?,
    ) else {
        return Ok(None);
    };
    Ok(page_sam(tiled, &boxes, width, height))
}

fn page_sam(tiled: Vec<u8>, boxes: &[u8], width: u32, height: u32) -> Option<PageSam> {
    let boxes: Vec<(i64, i64, u32, u32, u8, f32)> = serde_json::from_slice(boxes).ok()?;
    let rt: Vec<BalloonBox> = boxes
        .into_iter()
        .map(|(x, y, w, h, class, score)| BalloonBox {
            rect: Rect::new(x, y, w, h),
            class: [BalloonClass::Bubble, BalloonClass::TextInBubble, BalloonClass::TextFree][class as usize],
            score,
        })
        .collect();
    // The folder is the grouping measurement's cache (`GROUPING_CACHE`,
    // `text_groups::measure`): `{stem}.tiled.bin` is SAM-TS-L run on this
    // machine over the 1.0.0 cloud plan, whose tiles did not overlap, so
    // evidence changes owner on every multiple of the tile side; `{stem}.rt.json`
    // is Ogkalu Full run on this machine over two horizontal halves.
    let seams = |extent: u32| (1..).map(|k| k * cleaner_core::cloud_analysis_wire::MAX_TILE_SIDE).take_while(|&at| at < extent).collect::<Vec<u32>>();
    let (xs, ys) = (seams(width), seams(height));
    let mut inputs = Inputs::new(width, height, Some(&tiled));
    inputs.rt = &rt;
    inputs.models = vec![
        ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::CloudTiles),
        ModelUse::new(EvidenceModel::OgkaluFull, Execution::Local, SpatialInput::Halves),
    ];
    inputs.seams = xs
        .iter()
        .map(|&x| text_groups::Seam::Vertical(i64::from(x)))
        .chain(ys.iter().map(|&y| text_groups::Seam::Horizontal(i64::from(y))))
        .chain((height >= 2).then(|| text_groups::Seam::Horizontal(i64::from(height / 2))))
        .collect();
    let grouping = text_groups::group(&inputs).ok()?;
    Some(PageSam { width, tiled: tiled.clone(), grouping })
}

pub fn main() -> Result<(), String> {
    let config = Config {
        job: env_path("MC_STAGES_JOB").ok_or("MC_STAGES_JOB is required")?,
        attempts: env_path("MC_STAGES_ATTEMPTS").ok_or("MC_STAGES_ATTEMPTS is required")?,
        out: env_path("MC_STAGES_OUT").ok_or("MC_STAGES_OUT is required")?,
        snapshot: env_path("MC_STAGES_SNAPSHOT"),
        sam: env_path("MC_STAGES_SAM"),
        regions: std::env::var("MC_STAGES_REGIONS")
            .ok()
            .map(|list| list.split(',').map(str::to_owned).collect()),
        live: live_roots(),
    };
    let written = run(&config)?;
    println!("{written} regions written to {}", config.out.display());
    Ok(())
}

/// Capture every cached result `config` names. Answers how many regions the
/// summary lists. Nothing is read before every input is checked, and nothing
/// is written outside `config.out`.
pub fn run(config: &Config) -> Result<usize, String> {
    let guard = Guard::new(&config.live);
    // Every input, resolved. The chapter is opened at its resolved path, so
    // its sidecar folder is the one checked.
    let job_path = guard.resolve(&config.job)?;
    let attempts = guard.resolve(&config.attempts)?;
    let snapshot_path = config.snapshot.as_deref().map(|path| guard.resolve(path)).transpose()?;
    let sam_dir = config.sam.as_deref().map(|path| guard.resolve(path)).transpose()?;
    guard.check_tree(&attempts)?;
    if let Some(dir) = sam_dir.as_deref() {
        guard.check_tree(dir)?;
    }
    let only = config.regions.as_ref();

    let job = Job::open_read_only(&job_path).map_err(|e| e.to_string())?;
    guard.check_job(&job)?;
    let snapshot = snapshot_path
        .as_ref()
        .map(|path| Job::open_read_only(path).map_err(|e| e.to_string()))
        .transpose()?;
    if let Some(snapshot) = snapshot.as_ref() {
        guard.check_job(snapshot)?;
    }
    let inputs: Vec<PathBuf> = [Some(job.root().to_path_buf()), Some(attempts.clone()),
        snapshot.as_ref().map(|s| s.root().to_path_buf()), sam_dir.clone()]
        .into_iter()
        .flatten()
        .collect();
    let out = Output::new(&guard, &config.out, &inputs)?;

    let mut pages: HashMap<usize, (Raster, Option<PageSam>)> = HashMap::new();
    let mut names: Vec<PathBuf> = std::fs::read_dir(&attempts)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    names.sort();
    let mut summary = Vec::new();
    for dir in names {
        let Some(journal) = guard.read_if_present(&dir.join("journal.json"))? else { continue };
        let Some(result) = guard.read_if_present(&dir.join("result.png"))? else { continue };
        let journal: Value = serde_json::from_slice(&journal).map_err(|e| e.to_string())?;
        let region_id = journal["region_id"].as_str().unwrap_or_default().to_owned();
        if only.is_some_and(|ids| !ids.contains(&region_id)) {
            continue;
        }
        if !safe_name(&region_id) {
            summary.push(json!({ "region": region_id, "skipped": "region id is not a plain folder name" }));
            continue;
        }
        let page_index = journal["page_index"].as_u64().unwrap_or(0) as usize;
        let Some(source_idx) = job.project.strip.order.get(page_index).copied() else { continue };
        if let std::collections::hash_map::Entry::Vacant(slot) = pages.entry(source_idx) {
            let path = job.source_path(source_idx).ok_or("source missing")?;
            let raw = cleaner_core::image::decode(&guard.read(&path)?).map_err(|e| e.to_string())?;
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let sam = match sam_dir.as_ref() {
                Some(dir) => load_sam(&guard, dir, &stem, raw.width, raw.height)?,
                None => None,
            };
            slot.insert((raw, sam));
        }
        let (raw, sam) = &pages[&source_idx];

        // The region as it stood when the request was made: the snapshot's
        // detection, prepared first against the snapshot (no predecessors)
        // and then against the current chapter, whichever reproduces the
        // journal's digests.
        let detection = snapshot
            .as_ref()
            .and_then(|s| s.load_detection(&region_id).ok().flatten())
            .or_else(|| job.load_detection(&region_id).ok().flatten());
        let Some(detection) = detection else {
            summary.push(json!({ "region": region_id, "skipped": "no detection record" }));
            continue;
        };
        let Ok(preprocessing) = Preprocessing::of(journal["preprocessing_version"].as_str().unwrap_or("")) else {
            summary.push(json!({ "region": region_id, "skipped": "unsupported preprocessing version" }));
            continue;
        };
        let mut chosen = None;
        for (label, base) in [("snapshot", snapshot.as_ref()), ("current", Some(&job))] {
            let Some(base) = base else { continue };
            let hole = preprocessing.hole(&detection.mask, &detection.ink);
            let Ok(cloud) = crate::underlay::prepare_cloud_seed(
                base, source_idx, raw, detection.record.order, hole, preprocessing,
            ) else {
                continue;
            };
            let crop = cloud.prepared.crop();
            let crop_png = crate::inference::consent::encode_rgb8_png(crop.w, crop.h, cloud.prepared.image_rgb8())
                .map_err(|e| e.to_string())?;
            let hint_png = crate::inference::consent::encode_gray8_png(crop.w, crop.h, cloud.prepared.hint_gray8())
                .map_err(|e| e.to_string())?;
            let crop_ok = cleaner_core::ingest::sha256_hex(&crop_png) == journal["crop_sha256"].as_str().unwrap_or("");
            let hint_ok = cleaner_core::ingest::sha256_hex(&hint_png) == journal["hint_sha256"].as_str().unwrap_or("");
            let better = chosen.as_ref().is_none_or(|(_, _, c, h)| (crop_ok, hint_ok) > (*c, *h));
            if better {
                chosen = Some((label, cloud, crop_ok, hint_ok));
            }
        }
        let Some((basis, cloud, crop_ok, hint_ok)) = chosen else {
            summary.push(json!({ "region": region_id, "skipped": "preparation failed" }));
            continue;
        };
        let decoded = cleaner_core::image::decode(&result).map_err(|e| e.to_string())?;
        if decoded.mode != ColorMode::Rgb || decoded.depth != BitDepth::Eight {
            summary.push(json!({ "region": region_id, "skipped": "result is not RGB8" }));
            continue;
        }
        let prepared = &cloud.prepared;
        let crop = prepared.crop();
        let generated = GeneratedCrop::new(decoded.width, decoded.height, &decoded.data);
        let rendered = prepared.composite(&generated).map_err(|e| e.to_string())?;
        let tone = prepared.tone(&generated).map_err(|e| e.to_string())?;
        let aligned = prepared.blend(&generated, Some(tone.clone())).map_err(|e| e.to_string())?;
        // Window to page.
        let (wx, wy) = (cloud.input.window.x + cloud.source_shift.0, cloud.input.window.y + cloud.source_shift.1);
        let page_crop = Rect::new(crop.x + wx, crop.y + wy, crop.w, crop.h);

        // A region captured from an earlier attempt keeps its folder; this
        // one takes the next free name after it.
        let folder = (1..)
            .map(|n| if n == 1 { region_id.clone() } else { format!("{region_id}.{n}") })
            .find(|name| !out.root.join(name).exists())
            .expect("a free folder name");
        let stage = out.stage(&folder)?;
        out.png(&stage, "source", &page_rgb(raw, page_crop)?)?;
        let input = rgb(crop.w, crop.h, prepared.image_rgb8().to_vec());
        out.png(&stage, "input", &input)?;
        out.png(&stage, "hint", &gray(crop.w, crop.h, prepared.hint_gray8().to_vec()))?;
        out.png(&stage, "raw", &decoded)?;
        out.png(&stage, "det_ink", &mask_gray(&detection.ink, page_crop))?;
        out.png(&stage, "det_mask", &mask_gray(&detection.mask, page_crop))?;
        let mut alpha = vec![0u8; crop.w as usize * crop.h as usize];
        for y in 0..crop.h as i64 {
            for x in 0..crop.w as i64 {
                alpha[(y * crop.w as i64 + x) as usize] = (prepared.alpha(crop.x + x, crop.y + y) * 255.0).round() as u8;
            }
        }
        out.png(&stage, "alpha", &gray(crop.w, crop.h, alpha))?;
        let mut composite = input.clone();
        paint(&mut composite, crop, &rendered.pixels, rendered.mask.bounds)?;
        out.png(&stage, "composite", &composite)?;
        let mut toned = input.clone();
        paint(&mut toned, crop, &aligned.pixels, aligned.mask.bounds)?;
        out.png(&stage, "aligned", &toned)?;

        // What the current preprocessing would send for the same record, cut
        // from the basis that reproduced the journal.
        let base = if basis == "snapshot" { snapshot.as_ref().unwrap_or(&job) } else { &job };
        let current = Preprocessing::CURRENT;
        let mut v2_crop = Value::Null;
        if let Ok(v2) = crate::underlay::prepare_cloud_seed(
            base, source_idx, raw, detection.record.order,
            current.hole(&detection.mask, &detection.ink), current,
        ) {
            let c = v2.prepared.crop();
            out.png(&stage, "v2_input", &rgb(c.w, c.h, v2.prepared.image_rgb8().to_vec()))?;
            let mut alpha = vec![0u8; c.w as usize * c.h as usize];
            for y in 0..c.h as i64 {
                for x in 0..c.w as i64 {
                    alpha[(y * c.w as i64 + x) as usize] = (v2.prepared.alpha(c.x + x, c.y + y) * 255.0).round() as u8;
                }
            }
            out.png(&stage, "v2_alpha", &gray(c.w, c.h, alpha))?;
            v2_crop = json!({ "w": c.w, "h": c.h, "applied": v2.prepared.applied().count() });
        }

        // The patch as saved, in window coordinates.
        let stored = job
            .project
            .patches
            .iter()
            .find(|record| record.id == region_id)
            .and_then(|record| job.load_patch(record).ok());
        let mut matches_stored = Value::Null;
        if let Some(patch) = stored.as_ref() {
            let local = shifted(patch.mask.clone(), -wx, -wy);
            let mut saved = input.clone();
            paint(&mut saved, crop, &patch.pixels, local.bounds)?;
            out.png(&stage, "stored", &saved)?;
            matches_stored = json!(local == rendered.mask && patch.pixels.data == rendered.pixels.data);
        }
        let mut sam_counts = Value::Null;
        if let Some(sam) = sam {
            let mut lettering = Mask::empty(page_crop);
            for y in 0..page_crop.h as i64 {
                for x in 0..page_crop.w as i64 {
                    let (px, py) = (page_crop.x + x, page_crop.y + y);
                    if px >= 0 && py >= 0 && px < raw.width as i64 && py < raw.height as i64
                        && sam.tiled[py as usize * sam.width as usize + px as usize] != 0
                    {
                        lettering.set(px, py, true);
                    }
                }
            }
            out.png(&stage, "sam", &mask_gray(&lettering, page_crop))?;
            let mut group = Mask::empty(page_crop);
            let mut ids = Vec::new();
            for held in sam.grouping.cleaning() {
                let letters = sam.grouping.lettering(held);
                if letters.intersects(&detection.ink) {
                    ids.push(held.id.clone());
                    for y in 0..page_crop.h as i64 {
                        for x in 0..page_crop.w as i64 {
                            if letters.contains(page_crop.x + x, page_crop.y + y) {
                                group.set(page_crop.x + x, page_crop.y + y, true);
                            }
                        }
                    }
                }
            }
            out.png(&stage, "group", &mask_gray(&group, page_crop))?;
            sam_counts = json!({ "samPixels": lettering.count(), "groupPixels": group.count(), "groups": ids });
        }
        let meta = json!({
            "region": region_id,
            "folder": folder,
            "attempt": journal["attempt_id"],
            "phase": journal["phase"]["phase"],
            "page": page_index + 1,
            "basis": basis,
            "cropMatchesJournal": crop_ok,
            "hintMatchesJournal": hint_ok,
            "compositeMatchesStored": matches_stored,
            "cropPage": page_crop,
            "applied": prepared.applied().count(),
            "detInk": detection.ink.count(),
            "detMask": detection.mask.count(),
            "fit": detection.record.fit,
            "sam": sam_counts,
            // The exact models behind the `sam` and `group` stages, and what
            // rendered `raw`: the journal's recipe, model and revision, and
            // the preprocessing it was prepared and composited under.
            "models": sam.as_ref().map(|sam| json!(text_groups::describe_models(&sam.grouping.models))),
            "render": {
                "recipe": journal["recipe_id"],
                "model": journal["model_id"],
                "revision": journal["model_revision"],
                "preprocessing": preprocessing.version(),
                "currentPreprocessing": Preprocessing::CURRENT.version(),
            },
            "preprocessing": preprocessing.version(),
            "tone": tone,
            "v2": v2_crop,
        });
        out.write(&stage, "meta.json", &serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?)?;
        summary.push(meta);
    }
    out.write(&out.root, "summary.json", &serde_json::to_vec_pretty(&summary).map_err(|e| e.to_string())?)?;
    Ok(summary.len())
}

#[cfg(test)]
mod tests {
    #[test]
    fn diagnostic_pages_and_patches_use_managed_color() {
        let mut native=cleaner_core::image::fixtures::by_name("rgb8").raster;
        native.color.gamma=Some(100000);
        for c in 0..3 { native.set_sample(0,0,c,64); }
        let rect=cleaner_core::mask::Rect::new(0,0,1,1);
        let mut shown=super::page_rgb(&native,rect).unwrap();
        assert_eq!(shown.data,vec![137;3]);
        assert_eq!(shown.srgb_intent,Some(1));
        for c in 0..3 { native.set_sample(0,0,c,128); }
        super::paint(&mut shown,rect,&native,rect).unwrap();
        assert_eq!(shown.data,vec![188;3]);
    }

    use super::*;
    use cleaner_core::project::{DetectedRegion, Project, StripMode};
    use std::collections::BTreeMap;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mc-mask-stages-{name}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::canonicalize(root).unwrap()
    }

    /// A copied chapter under `root`: one 256 px page with a dark block and a
    /// detection over it, and an attempt folder holding a white result of the
    /// crop the detection's render cuts. Answers the manifest and the
    /// attempts folder.
    fn copied_chapter(root: &Path, region_id: &str) -> (PathBuf, PathBuf) {
        let mut page = cleaner_core::image::fixtures::by_name("l8").raster;
        (page.width, page.height) = (256, 256);
        page.data = vec![200; 256 * 256];
        for y in 100..112 {
            for x in 100..130 {
                page.set_sample(x, y, 0, 20);
            }
        }
        std::fs::create_dir_all(root.join("raws")).unwrap();
        let bytes = cleaner_core::image::encode(&page, Format::Png).unwrap();
        let source = root.join("raws/page.png");
        std::fs::write(&source, &bytes).unwrap();
        let source = cleaner_core::ingest::source_ref(&source, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let mut job = Job::create(&manifest, Project::new(manifest.parent().unwrap(), "test", StripMode::Single,
            &[source])).unwrap();
        let bbox = Rect::new(98, 98, 34, 16);
        let record = DetectedRegion {
            id: region_id.into(), source_idx: 0, bbox, mask_ref: String::new(), inside: true, balloon_color: None,
            script: Some("ja".into()), pick: "lama".into(), detector: "local".into(), created: 1_760_000_000,
            mask_sha256: String::new(), order: 0, review_state: None, fit: None, group: None,
            padding_from_seed: false,
            padding_px: 0,
        };
        job.store_detection(record, &Mask::filled(bbox), &Mask::filled(bbox)).unwrap();

        let detection = job.load_detection(region_id).unwrap().unwrap();
        let v2 = Preprocessing::V2;
        let cloud = crate::underlay::prepare_cloud_seed(&job, 0, &page, 0,
            v2.hole(&detection.mask, &detection.ink), v2).unwrap();
        let crop = cloud.prepared.crop();
        let attempt = root.join("attempts/att-1");
        std::fs::create_dir_all(&attempt).unwrap();
        let result = Raster { width: crop.w, height: crop.h, mode: ColorMode::Rgb, depth: BitDepth::Eight,
            icc: None, palette: None, trns: None, srgb_intent: None, color: Default::default(), data: vec![255; (crop.w * crop.h * 3) as usize] };
        std::fs::write(attempt.join("result.png"), cleaner_core::image::encode(&result, Format::Png).unwrap()).unwrap();
        std::fs::write(attempt.join("journal.json"), serde_json::to_vec(&json!({
            "attempt_id": "att-1", "region_id": region_id, "page_index": 0, "preprocessing_version": "2.0.0",
            "crop_sha256": "", "hint_sha256": "", "phase": { "phase": "committed" },
        })).unwrap()).unwrap();
        drop(job);
        // Setting the fixture up took the job's lock; a capture must not.
        let _ = std::fs::remove_file(cleaner_core::project::lock::lock_path(&manifest));
        (manifest, root.join("attempts"))
    }

    fn config(job: &Path, attempts: &Path, out: &Path, live: &Path) -> Config {
        Config {
            job: job.to_path_buf(), attempts: attempts.to_path_buf(), out: out.to_path_buf(), snapshot: None,
            sam: None, regions: None, live: vec![live.to_path_buf()],
        }
    }

    /// Every file under `dir`, and the digest of its bytes.
    fn tree(dir: &Path) -> BTreeMap<PathBuf, String> {
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(tree(&path));
            } else {
                files.insert(path.clone(), cleaner_core::ingest::sha256_hex(&std::fs::read(&path).unwrap()));
            }
        }
        files
    }

    /// **A capture reads its input and changes nothing in it**: no lock file
    /// appears beside the chapter and an orphaned detection file the store
    /// would sweep on an ordinary open is still there, byte for byte.
    #[test]
    fn a_capture_leaves_its_input_byte_for_byte() {
        let root = scratch("untouched");
        let input = root.join("copy");
        let (manifest, attempts) = copied_chapter(&input, "chapter-p001-d0");
        let orphan = manifest.with_extension("mtclean.d").join("detections/orphan.mask");
        std::fs::write(&orphan, b"not named by the manifest").unwrap();
        let before = tree(&input);

        let out = root.join("stages");
        let written = run(&config(&manifest, &attempts, &out, &root.join("live"))).unwrap();
        assert_eq!(written, 1);
        assert!(out.join("chapter-p001-d0/meta.json").is_file());
        assert!(out.join("chapter-p001-d0/aligned.png").is_file());
        assert_eq!(tree(&input), before, "the capture changed its input");
    }

    /// `meta.json` names the exact models behind the `sam` and `group` stages
    /// (the measurement cache ran both here: SAM-TS-L over the 1.0.0 tiles,
    /// Ogkalu Full over two halves) and the render behind `raw`.
    #[test]
    fn a_capture_names_its_models_and_its_render() {
        let root = scratch("named");
        let (manifest, attempts) = copied_chapter(&root.join("copy"), "chapter-p001-d0");
        let journal = attempts.join("att-1/journal.json");
        let mut record: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        record["recipe_id"] = json!("recipe-test");
        record["model_id"] = json!("model-test");
        record["model_revision"] = json!("revision-test");
        std::fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        let sam = root.join("sam");
        std::fs::create_dir_all(&sam).unwrap();
        let mut mask = vec![0u8; 256 * 256];
        for y in 100..112 {
            mask[y * 256 + 100..y * 256 + 130].fill(255);
        }
        std::fs::write(sam.join("page.tiled.bin"), &mask).unwrap();
        std::fs::write(sam.join("page.rt.json"), b"[[96,96,40,20,2,0.9]]").unwrap();
        let out = root.join("stages");
        let config = Config { sam: Some(sam), ..config(&manifest, &attempts, &out, &root.join("live")) };
        assert_eq!(run(&config).unwrap(), 1);
        let meta: Value = serde_json::from_slice(&std::fs::read(out.join("chapter-p001-d0/meta.json")).unwrap()).unwrap();
        let models = &meta["models"];
        assert_eq!(models[0]["checkpoint"], "mayocream/koharu-text-sam-ts-l");
        assert_eq!(models[0]["revision"], "5dd97423e0fbf2404264979136d47e8101144046");
        assert_eq!(models[0]["execution"], "local");
        assert_eq!(models[0]["spatial"], "cloud tiles 1.0.0: non-overlapping tiles of at most 1024");
        assert_eq!(models[1]["checkpoint"], "ogkalu/comic-text-and-bubble-detector detector.onnx (RT-DETR v2, FP32)");
        assert_eq!(models[1]["spatial"], "two horizontal halves");
        assert_eq!(meta["render"], json!({
            "recipe": "recipe-test", "model": "model-test", "revision": "revision-test",
            "preprocessing": "2.0.0", "currentPreprocessing": Preprocessing::CURRENT.version(),
        }));
        assert!(!meta.to_string().contains("koharu-layout") && !meta.to_string().contains("manga-text-segmentation"));
    }

    /// **The live data is refused however it is named**: through a link to
    /// it, through `..`, or through a copied manifest whose source points
    /// into it. Nothing is written when it is.
    #[test]
    fn live_data_is_refused_through_a_link_a_dotdot_or_a_manifest_reference() {
        let root = scratch("live");
        let live = root.join("app-data");
        let (_, attempts) = copied_chapter(&live.join("library/p1"), "chapter-p001-d0");
        let out = root.join("stages");

        // A symbolic link needs a privilege Windows grants only in developer
        // mode, so the link case runs where links are ordinary.
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(live.join("library"), &alias).unwrap();
            let through_link = alias.join("p1/job/chapter.mtclean");
            assert!(through_link.is_file());
            let error = run(&config(&through_link, &attempts, &out, &live)).unwrap_err();
            assert!(error.contains("live application data"), "{error}");
        }

        let dotdot = root.join("elsewhere/../app-data/library/p1/job/chapter.mtclean");
        std::fs::create_dir_all(root.join("elsewhere")).unwrap();
        assert!(run(&config(&dotdot, &attempts, &out, &live)).unwrap_err().contains("live application data"));

        // A copy outside the live data, its attempts too, whose manifest
        // still names the live page.
        let copy = root.join("copy");
        let (copied, copied_attempts) = copied_chapter(&copy, "chapter-p001-d0");
        let mut project: Value = serde_json::from_slice(&std::fs::read(&copied).unwrap()).unwrap();
        project["sources"][0]["rel_path"] = json!("../../app-data/library/p1/raws/page.png");
        std::fs::write(&copied, serde_json::to_vec(&project).unwrap()).unwrap();
        let error = run(&config(&copied, &copied_attempts, &out, &live)).unwrap_err();
        assert!(error.contains("live application data"), "{error}");

        // The live attempts, read through a link from the copy's side.
        let (clean, _) = copied_chapter(&root.join("clean"), "chapter-p001-d0");
        #[cfg(unix)]
        {
            let linked_attempts = root.join("attempts-link");
            std::os::unix::fs::symlink(live.join("library/p1/attempts"), &linked_attempts).unwrap();
            assert!(run(&config(&clean, &linked_attempts, &out, &live)).unwrap_err().contains("live application data"));
        }

        // An output inside the live data is refused before it is made.
        let inside = live.join("stages");
        assert!(run(&config(&clean, &root.join("clean/attempts"), &inside, &live)).is_err());
        assert!(!inside.exists());
        assert!(!out.exists(), "a refused capture wrote its output");
    }

    /// **Nothing is written through a file that was already there.** An
    /// output folder that exists with anything in it is refused, whatever it
    /// holds: here a `summary.json` hard-linked to a live manifest, which a
    /// resolved-path check cannot tell from a plain file. Inside a fresh
    /// output every file is created new and never through a link, so one
    /// planted after the folder was made is refused too. A hard-linked live
    /// input is read and nothing else: it is left byte for byte.
    #[test]
    fn a_capture_never_writes_through_a_file_or_link_it_did_not_make() {
        let root = scratch("hard-link");
        let live = root.join("app-data");
        let (live_manifest, live_attempts) = copied_chapter(&live.join("library/p1"), "chapter-p001-d0");
        let live_before = tree(&live);
        let (manifest, attempts) = copied_chapter(&root.join("copy"), "chapter-p001-d0");

        let out = root.join("stages");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::hard_link(&live_manifest, out.join("summary.json")).unwrap();
        let error = run(&config(&manifest, &attempts, &out, &live)).unwrap_err();
        assert!(error.contains("not empty"), "{error}");
        assert_eq!(tree(&live), live_before, "the live manifest was written through its hard link");

        // A fresh folder is made and filled; a second capture into it is
        // refused rather than overwriting the first.
        let fresh = root.join("fresh");
        assert_eq!(run(&config(&manifest, &attempts, &fresh, &live)).unwrap(), 1);
        assert!(run(&config(&manifest, &attempts, &fresh, &live)).unwrap_err().contains("not empty"));

        // Planted inside an output after it was made: a hard link, a link.
        let output = Output::new(&Guard::new(std::slice::from_ref(&live)), &root.join("empty"), &[]).unwrap();
        std::fs::hard_link(&live_manifest, output.root.join("planted.json")).unwrap();
        assert!(output.write(&output.root, "planted.json", b"{}").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&live_manifest, output.root.join("linked.json")).unwrap();
            assert!(output.write(&output.root, "linked.json", b"{}").is_err());
        }
        assert_eq!(tree(&live), live_before);

        // A live attempt hard-linked into the copy resolves to the copy's own
        // path, and is only ever read.
        let copied_attempt = attempts.join("att-1");
        for name in ["result.png", "journal.json"] {
            std::fs::remove_file(copied_attempt.join(name)).unwrap();
            std::fs::hard_link(live_attempts.join("att-1").join(name), copied_attempt.join(name)).unwrap();
        }
        assert_eq!(run(&config(&manifest, &attempts, &root.join("from-links"), &live)).unwrap(), 1);
        assert_eq!(tree(&live), live_before, "a hard-linked live input was written");
    }

    /// **A region id writes only inside the output.** A journal and manifest
    /// whose region is `../escape` are a region to skip, not a folder beside
    /// the output to write into; a manifest naming a sidecar file by `..` is
    /// refused outright.
    #[test]
    fn a_region_id_that_is_not_a_plain_name_writes_nothing_outside_the_output() {
        let root = scratch("escape");
        let copy = root.join("copy");
        let (manifest, attempts) = copied_chapter(&copy, "../escape");
        let live = root.join("live");
        let out = root.join("out/stages");

        // As stored, the detection's own files are named through `..`.
        let error = run(&config(&manifest, &attempts, &out, &live)).unwrap_err();
        assert!(error.contains("outside its own folder"), "{error}");

        // Named plainly, the region's id is still no folder name.
        let mut project: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        project["detections"][0]["mask_ref"] = json!("detections/escape.mask");
        std::fs::write(&manifest, serde_json::to_vec(&project).unwrap()).unwrap();
        let sidecar = manifest.with_extension("mtclean.d");
        std::fs::rename(sidecar.join("escape.mask"), sidecar.join("detections/escape.mask")).unwrap();
        std::fs::rename(sidecar.join("escape.ink"), sidecar.join("detections/escape.ink")).unwrap();
        assert_eq!(run(&config(&manifest, &attempts, &out, &live)).unwrap(), 1);
        assert!(!root.join("out/escape").exists(), "a region id wrote beside the output");
        let summary: Value = serde_json::from_slice(&std::fs::read(out.join("summary.json")).unwrap()).unwrap();
        assert_eq!(summary[0]["region"], "../escape");
        assert!(summary[0]["skipped"].is_string());
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "only the summary");

        for name in ["", ".", "..", "a/b", "../x", "a\\b"] {
            assert!(!safe_name(name), "{name:?}");
        }
        assert!(safe_name("c32-p004-d10"));
    }
}
