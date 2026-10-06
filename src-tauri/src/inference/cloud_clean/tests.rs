use super::*;
use std::cell::RefCell;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, LazyLock};

use cleaner_core::image::{fixtures, Format, Raster};
use cleaner_core::mask::Mask;
use cleaner_core::patch::{Engine, Patch, Provenance};
use cleaner_core::project::{DetectedRegion, Project};
use cleaner_core::text_groups::{GroupOrigin, GroupRecord, GROUPING_VERSION};

use crate::inference::journal::{AttemptJournal, CreateAttemptIntent};

static FINGERPRINT: LazyLock<String> = LazyLock::new(|| "f".repeat(64));

const JOB_REQUEST_FIXTURE: &str = include_str!("../../../../deploy/cloud/fixtures/job_request_metadata.json");

fn page_raster() -> Raster {
    let mut raster = fixtures::by_name("l8").raster;
    raster.width = 256;
    raster.height = 256;
    raster.data = vec![200; 256 * 256];
    raster
}

/// A chapter of `pages` flat 256 by 256 pages, each with its own shade so no
/// two pages share a digest.
fn chapter(name: &str, pages: usize, mode: StripMode) -> PathBuf {
    chapter_of(name, pages, mode, 256, 256)
}

/// [`chapter`] with `w` by `h` pages.
fn chapter_of(name: &str, pages: usize, mode: StripMode, w: u32, h: u32) -> PathBuf {
    let root = std::env::temp_dir().join(format!("mc-cloud-clean-{name}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let references: Vec<_> = (0..pages).map(|index| {
        let mut page = page_raster();
        (page.width, page.height) = (w, h);
        page.data = vec![100 + index as u8; w as usize * h as usize];
        let source = root.join(format!("page{index}.png"));
        let bytes = cleaner_core::image::encode(&page, Format::Png).unwrap();
        std::fs::write(&source, &bytes).unwrap();
        cleaner_core::ingest::source_ref(&source, &bytes).unwrap()
    }).collect();
    let manifest = root.join("job/chapter.mtclean");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    Job::create(&manifest, Project::new(manifest.parent().unwrap(), "test-app", mode, &references)).unwrap();
    manifest
}

/// A stored pick the mixed local pass does not take, so the cloud renders it.
/// Denoise fill was the real pick that did this; with it removed, a stored
/// `denoise` reads as a fill, and these tests name a rung the local pass has
/// no arm for instead.
const REMOTE_PICK: &str = "flux";

fn record(id: String, page: usize, bbox: Rect, pick: &str, order: u32) -> DetectedRegion {
    DetectedRegion {
        id, source_idx: page, bbox, mask_ref: String::new(), inside: true,
        balloon_color: Some([200, 200, 200]), script: Some("ja".into()), pick: pick.into(),
        detector: "local".into(), created: 1_760_000_000, mask_sha256: String::new(), order,
        review_state: None, fit: None, group: None,
        padding_from_seed: false,
        padding_px: 0,
    }
}

fn group(id: &str, bounds: Rect) -> GroupRecord {
    GroupRecord {
        version: GROUPING_VERSION, id: id.into(), origin: GroupOrigin::TextBox, bounds,
        lettering_pixels: bounds.w * bounds.h, lettering_sha256: "b".repeat(64),
        components: vec!["sam-00001".into()], boxes: Vec::new(), bubble: None, models: Vec::new(),
        reasons: Vec::new(), split: None, estimated: false, evidence_ref: None, evidence_sha256: None,
    }
}

/// Store one detection whose mask is its box.
fn detect(path: &Path, page: usize, bbox: Rect, pick: &str, order: u32) -> String {
    let mut job = Job::open(path).unwrap();
    let id = job.next_detection_id(&library::page_id("chapter", page));
    job.store_detection(record(id.clone(), page, bbox, pick, order), &Mask::filled(bbox), &Mask::filled(bbox))
        .unwrap();
    library::invalidate_manifest_cache(path);
    id
}

/// Store many detections in one manifest write, each its own text group.
fn detect_many(path: &Path, rows: &[(usize, Rect, u32)]) -> Vec<String> {
    let mut job = Job::open(path).unwrap();
    let dir = job.sidecar().join("detections");
    std::fs::create_dir_all(&dir).unwrap();
    let ids: Vec<String> = rows.iter().map(|&(page, bbox, order)| {
        let id = job.next_detection_id(&library::page_id("chapter", page));
        let bytes = cleaner_core::project::buffers::encode_mask(&Mask::filled(bbox));
        std::fs::write(dir.join(format!("{id}.mask")), &bytes).unwrap();
        std::fs::write(dir.join(format!("{id}.ink")), &bytes).unwrap();
        let mut row = record(id.clone(), page, bbox, REMOTE_PICK, order);
        row.mask_ref = format!("detections/{id}.mask");
        row.mask_sha256 = sha256_hex(&bytes);
        row.group = Some(group(&format!("tg-{:012x}", job.project.detections.len()), bbox));
        job.project.detections.push(row);
        id
    }).collect();
    job.flush().unwrap();
    library::invalidate_manifest_cache(path);
    ids
}

fn recipe() -> RenderRecipe {
    RenderRecipe::new("sdnq-v1", "1.0.0", "flux-schnell", "0123456789abcdef0123456789abcdef01234567", false)
}

fn l4() -> Option<(String, f64)> { Some(("L4".into(), 0.8)) }

struct Gateway {
    price: Option<(String, f64)>,
    recipe: RenderRecipe,
    asked: AtomicUsize,
    priced: AtomicUsize,
}

impl Gateway {
    fn priced() -> Self {
        Self { price: l4(), recipe: recipe(), asked: AtomicUsize::new(0), priced: AtomicUsize::new(0) }
    }
}

impl CleanGateway for Gateway {
    fn recipe(&self) -> Result<RenderRecipe, String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(self.recipe.clone())
    }
    fn gpu_price(&self) -> Option<(String, f64)> {
        self.priced.fetch_add(1, Ordering::SeqCst);
        self.price.clone()
    }
}

fn destination(profile: &str) -> Destination {
    Destination {
        provider: CloudProvider::Modal, profile_id: profile.into(), profile_name: "Modal".into(),
        endpoint_fingerprint: FINGERPRINT.clone(),
    }
}

fn asked(scope: &str, local_first: bool) -> PrepareRequest {
    PrepareRequest {
        qwen_edit: None,
        chapter_id: "chapter".into(), scope: scope.into(), page_indices: None, region_ids: None, local_first,
        picks: None,
    }
}

/// Nothing unresolved.
fn settled(_: u32, _: &str) -> bool { false }

fn detection_ids(path: &Path) -> Vec<String> {
    Job::open(path).unwrap().project.detections.iter().map(|row| row.id.clone()).collect()
}

/* ------------------------------------------------------------------ */
/* Prepare                                                             */
/* ------------------------------------------------------------------ */

/// Strictly the cloud: every pick, a fill included, is planned for the cloud
/// GPU, and preparing writes nothing at all.
#[test]
fn preparing_describes_the_plan_and_cleans_nothing() {
    let path = chapter("describe", 2, StripMode::Single);
    let fill = detect(&path, 0, Rect::new(100, 100, 24, 16), "fill", 0);
    let remote0 = detect(&path, 0, Rect::new(40, 180, 30, 20), REMOTE_PICK, 1);
    let remote1 = detect(&path, 1, Rect::new(100, 100, 30, 20), REMOTE_PICK, 0);
    let before = std::fs::read(&path).unwrap();
    let gateway = Gateway::priced();

    let proposal = prepare(&path, asked("chapter", false), destination("modal-prof-clean-describe"), &gateway,
        &settled).unwrap();
    assert_eq!(proposal.execution, CleanExecution::Cloud);
    assert_eq!(proposal.region_ids, vec![fill.clone(), remote0.clone(), remote1.clone()]);
    assert_eq!((proposal.regions, proposal.pages, proposal.page_indices.clone()), (3, 2, vec![0, 1]));
    assert_eq!((proposal.local_cleaned, proposal.local_candidates), (0, 0));
    assert_eq!((proposal.chunk_regions, proposal.chunks), (CHUNK_REGIONS as u32, 1));
    assert!(proposal.unresolved_ids.is_empty());
    assert!(proposal.total_crop_pixels >= 3 * 30 * 20 && proposal.total_work_pixels > proposal.total_crop_pixels);
    let seconds = proposal.estimated_gpu_seconds.expect("a plan states its GPU time");
    assert!(seconds.low > 0.0 && seconds.high > seconds.low, "{seconds:?}");
    let cost = proposal.estimated_cost_usd.expect("a priced GPU gives a range");
    assert!(cost.low > 0.0 && cost.high > cost.low, "{cost:?}");
    assert_eq!(proposal.gpu.as_deref(), Some("L4"));
    assert_eq!(proposal.plan_digest.as_ref().map(String::len), Some(64));
    assert!(proposal.proposal_id.is_some() && proposal.expires_at_ms > 0);

    assert_eq!(std::fs::read(&path).unwrap(), before, "preparing wrote to the chapter");
    assert!(Job::open(&path).unwrap().project.patches.is_empty());
    assert_eq!(detection_ids(&path), vec![fill, remote0, remote1]);
}

/// Mixed is a choice made by name. Preparing it still only describes: the
/// fill is counted as a local candidate, stays in the plan for the cloud,
/// and nothing is cleaned until the run starts.
#[test]
fn mixed_is_asked_for_by_name_and_only_described_until_the_run() {
    let path = chapter("mixed-describe", 1, StripMode::Single);
    let fill = detect(&path, 0, Rect::new(100, 100, 24, 16), "fill", 0);
    let solid = detect(&path, 0, Rect::new(20, 20, 24, 16), "solid", 1);
    let remote = detect(&path, 0, Rect::new(40, 180, 30, 20), REMOTE_PICK, 2);
    let proposal = prepare(&path, asked("chapter", true), destination("modal-prof-clean-mixed"),
        &Gateway::priced(), &settled).unwrap();
    assert_eq!(proposal.execution, CleanExecution::Mixed);
    assert_eq!(proposal.local_candidates, 2);
    assert_eq!(proposal.local_cleaned, 0);
    assert_eq!(proposal.region_ids, vec![fill.clone(), solid.clone(), remote.clone()]);
    assert!(Job::open(&path).unwrap().project.patches.is_empty());
    assert_eq!(detection_ids(&path), vec![fill, solid, remote]);
}

#[test]
fn nothing_to_send_asks_the_gateway_nothing() {
    let empty = chapter("empty", 1, StripMode::Single);
    let gateway = Gateway::priced();
    let proposal = prepare(&empty, asked("chapter", false), destination("modal-prof-clean-none"), &gateway, &settled)
        .unwrap();
    assert_eq!((proposal.regions, proposal.chunks), (0, 0));
    assert!(proposal.proposal_id.is_none() && proposal.plan_digest.is_none());
    assert!(proposal.estimated_cost_usd.is_none() && proposal.estimated_gpu_seconds.is_none());
    assert_eq!(gateway.asked.load(Ordering::SeqCst) + gateway.priced.load(Ordering::SeqCst), 0);
}

#[test]
fn without_a_price_the_cost_is_null_and_the_gpu_time_is_still_stated() {
    let path = chapter("no-price", 1, StripMode::Single);
    let fill = detect(&path, 0, Rect::new(100, 100, 24, 16), "fill", 0);
    let gateway = Gateway { price: None, ..Gateway::priced() };
    let proposal = prepare(&path, asked("chapter", false), destination("modal-prof-clean-nolocal"), &gateway,
        &settled).unwrap();
    assert_eq!((proposal.regions, proposal.local_cleaned), (1, 0));
    assert_eq!(proposal.region_ids, vec![fill]);
    assert!(proposal.estimated_cost_usd.is_none() && proposal.gpu.is_none());
    assert!(proposal.estimated_gpu_seconds.is_some());
    assert!(proposal.proposal_id.is_some());
}

#[test]
fn scopes_name_pages_or_ids_and_everything_else_is_refused() {
    let path = chapter("scopes", 2, StripMode::Single);
    let first = detect(&path, 0, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let second = detect(&path, 1, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let run = |scope: &str, pages: Option<Vec<u32>>, ids: Option<Vec<String>>| {
        let request = PrepareRequest {
        qwen_edit: None, page_indices: pages, region_ids: ids, ..asked(scope, false) };
        prepare(&path, request, destination("modal-prof-clean-scope"), &Gateway::priced(), &settled)
    };
    assert_eq!(run("page", Some(vec![1]), None).unwrap().region_ids, vec![second.clone()]);
    assert_eq!(run("regions", None, Some(vec![first.clone()])).unwrap().region_ids, vec![first]);
    assert_eq!(run("regions", None, Some(vec!["nope".into()])).unwrap_err(), "cloud_clean_region_not_found");
    assert_eq!(run("regions", None, Some(vec![])).unwrap_err(), "cloud_clean_no_regions");
    assert_eq!(run("page", Some(vec![]), None).unwrap_err(), "cloud_clean_no_pages");
    assert_eq!(run("page", Some(vec![9]), None).unwrap_err(), "cloud_clean_page_not_found");
    assert_eq!(run("project", None, None).unwrap_err(), "cloud_clean_scope_unsupported");
}

/// Text cleanup's picks are this clean's: saved onto the regions before the
/// plan is bound, and a region the panel sets to LaMa is still planned for the
/// cloud. A region with an unresolved cloud request keeps its pick and its
/// revision.
#[test]
fn the_cleans_own_picks_are_saved_before_the_plan_and_held_regions_keep_theirs() {
    let path = chapter("repick", 1, StripMode::Single);
    let first = detect(&path, 0, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let held = detect(&path, 0, Rect::new(150, 150, 30, 20), REMOTE_PICK, 1);
    let picks = run::Repick { bubble: Some(run::EnginePick::Lama), outside: Some(run::EnginePick::Fill),
        ceiling: Engine::Lama };
    let request = PrepareRequest {
    qwen_edit: None, picks: Some(picks), ..asked("chapter", false) };
    let unresolved = |_: u32, id: &str| id == held;
    let proposal = prepare(&path, request, destination("modal-prof-clean-repick"), &Gateway::priced(), &unresolved)
        .unwrap();
    if let Some(id) = proposal.proposal_id.as_deref() { discard(id).unwrap(); }
    assert_eq!(proposal.region_ids, vec![first.clone()]);
    let job = Job::open(&path).unwrap();
    let pick = |id: &str| job.project.detections.iter().find(|row| row.id == id).unwrap().pick.clone();
    assert_eq!((pick(&first), pick(&held)), ("lama".to_string(), REMOTE_PICK.to_string()));
}

/// The region menu's mixed clean sends a flat-colour bubble pick alone: a
/// region inside a balloon takes it and becomes a local candidate, and one
/// outside keeps its saved pick and goes to the cloud as it would have.
#[test]
fn a_bubble_pick_alone_leaves_outside_regions_their_own() {
    let path = chapter("repick-side", 1, StripMode::Single);
    let bubble = detect(&path, 0, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let outside = detect(&path, 0, Rect::new(150, 150, 30, 20), REMOTE_PICK, 1);
    {
        let mut job = Job::open(&path).unwrap();
        job.project.detections.iter_mut().find(|row| row.id == outside).unwrap().inside = false;
        job.flush().unwrap();
        library::invalidate_manifest_cache(&path);
    }
    let picks = run::Repick::from_args(Some("fill"), None, Engine::Lama);
    assert_eq!(run::Repick::from_args(None, None, Engine::Lama), None);
    let request = PrepareRequest {
    qwen_edit: None, picks, ..asked("chapter", true) };
    let proposal = prepare(&path, request, destination("modal-prof-clean-side"), &Gateway::priced(), &settled)
        .unwrap();
    if let Some(id) = proposal.proposal_id.as_deref() { discard(id).unwrap(); }
    assert_eq!(proposal.local_candidates, 1);
    let job = Job::open(&path).unwrap();
    let pick = |id: &str| job.project.detections.iter().find(|row| row.id == id).unwrap().pick.clone();
    assert_eq!((pick(&bubble), pick(&outside)), ("fill".to_string(), REMOTE_PICK.to_string()));
}

/// A long strip is planned like a paginated chapter, and a region at the foot
/// of one page and one at the head of the next, which read each other across
/// the join, are both rendered from the strip and committed.
#[test]
fn a_long_strip_is_planned_and_cleaned_across_its_join() {
    let strip = chapter("strip", 2, StripMode::Longstrip);
    let foot = detect(&strip, 0, Rect::new(40, 226, 30, 20), REMOTE_PICK, 0);
    let head = detect(&strip, 1, Rect::new(40, 10, 30, 20), REMOTE_PICK, 0);
    let proposal = prepare(&strip, asked("chapter", false), destination("modal-prof-clean-strip"), &Gateway::priced(),
        &settled).unwrap();
    assert_eq!(proposal.region_ids, vec![foot.clone(), head.clone()]);
    // The proposal cache is shared by every test in the process.
    assert!(discard(proposal.proposal_id.as_deref().unwrap()).unwrap());

    let grants = Arc::new(GrantService::new());
    let scope = scope_for(&strip, &[foot.clone(), head.clone()], &grants);
    // Strip pixels: the second page starts 256 rows down.
    assert_eq!((scope.regions[0].space, scope.regions[1].space), (0, 0));
    assert_eq!(scope.regions[1].bbox, Rect::new(40, 266, 30, 20));
    let renderer = Renderer::new(&strip, &grants);
    let (summary, _) = run_scope(&scope, &strip, &grants, &renderer, &AtomicBool::new(false), &OPEN);
    assert_eq!((summary.reason, summary.regions_cleaned, summary.errored), ("completed", 2, 0));
    assert!(detection_ids(&strip).is_empty());
}

/// The plan bound is far above one chunk; past it the plan is refused before
/// a single region is read or the gateway asked.
#[test]
fn a_plan_past_the_plan_cap_is_refused_before_anything_is_read_or_asked() {
    let path = chapter("cap", 1, StripMode::Single);
    let mut job = Job::open(&path).unwrap();
    for n in 0..=MAX_PLAN_REGIONS {
        job.project.detections.push(record(format!("p-d{n}"), 0, Rect::new(10, 10, 4, 4), REMOTE_PICK, n as u32));
    }
    job.flush().unwrap();
    let gateway = Gateway::priced();
    // No mask file exists: reading one would fail with another error.
    assert_eq!(prepare(&path, asked("chapter", false), destination("modal-prof-clean-cap"), &gateway, &settled)
        .unwrap_err(), "cloud_clean_too_many_regions");
    assert_eq!(gateway.asked.load(Ordering::SeqCst), 0);
}

#[test]
fn proposal_capacity_refuses_before_the_gateway_is_asked() {
    let path = chapter("proposal-limit", 1, StripMode::Single);
    let id = detect(&path, 0, Rect::new(100, 100, 24, 16), "fill", 0);
    let scope = scope_for(&path, std::slice::from_ref(&id), &GrantService::new());
    let cache = Mutex::new(HashMap::new());
    let expires_at_ms = analysis::now_ms() + 60_000;
    for n in 0..MAX_OPEN_PROPOSALS {
        cache.lock().unwrap().insert(n.to_string(), StoredProposal { expires_at_ms, scope: scope.clone() });
    }
    let gateway = Gateway::priced();
    assert_eq!(prepare_with_cache(&path, asked("chapter", true), destination("modal-prof-limit"),
        &gateway, &settled, &cache).unwrap_err(), "cloud_clean_proposal_limit");
    assert_eq!(gateway.asked.load(Ordering::SeqCst), 0);
    assert_eq!(detection_ids(&path), vec![id]);
    assert!(Job::open(&path).unwrap().project.patches.is_empty());
}

/// A prepare waiting on its chapter's lock does not hold the proposal cache:
/// the chapter is read and bound before the cache is touched, so every other
/// proposal command goes on meanwhile.
#[test]
fn a_prepare_waiting_on_its_chapter_does_not_hold_the_proposal_cache() {
    let path = chapter("cache-free", 1, StripMode::Single);
    detect(&path, 0, Rect::new(40, 180, 30, 20), REMOTE_PICK, 0);
    let cache = Mutex::new(HashMap::new());
    let gateway = Gateway::priced();

    let held = run::lock_job(&path).unwrap();
    let (free, prepared) = std::thread::scope(|scope| {
        let prepare = scope.spawn(|| {
            prepare_with_cache(&path, asked("chapter", false), destination("modal-prof-cache-free"),
                &gateway, &settled, &cache)
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        let free = cache.try_lock().is_ok();
        drop(held);
        (free, prepare.join().unwrap())
    });

    assert!(free, "the proposal cache was held while the prepare waited on the chapter");
    let proposal = prepared.unwrap();
    let cache = cache.lock().unwrap();
    assert!(cache.contains_key(proposal.proposal_id.as_deref().unwrap()));
    assert_eq!(cache.len(), 1);
}

/// **The estimate is the crop the run will send.** The deployment's recipe
/// names the preprocessing its renders are cut with, so a 1.0.0 gateway is
/// estimated at 1.0.0's crops and a 2.0.0 gateway at 2.0.0's, each the crop
/// the render reads under that recipe; the plan digest binds the difference,
/// and a version this build cannot prepare is refused before any plan.
#[test]
fn the_estimate_follows_the_preprocessing_the_deployment_serves() {
    let path = chapter("recipe-crop", 1, StripMode::Single);
    let id = detect(&path, 0, Rect::new(100, 100, 30, 20), REMOTE_PICK, 0);
    let at = |version: &str| RenderRecipe { preprocessing_version: version.into(), ..recipe() };
    let mut digests = Vec::new();
    for version in ["1.0.0", cleaner_core::engines::render::PREPROCESSING_VERSION] {
        let gateway = Gateway { recipe: at(version), ..Gateway::priced() };
        let proposal = prepare(&path, asked("chapter", false), destination("modal-prof-clean-recipe"), &gateway,
            &settled).unwrap();
        let sent = read_render_input(&path, 0, &id, &at(version)).unwrap().crop_bounds;
        assert_eq!(proposal.total_crop_pixels, u64::from(sent.w) * u64::from(sent.h), "{version}");
        digests.push((proposal.total_crop_pixels, proposal.plan_digest.unwrap()));
    }
    assert!(digests[0].0 < digests[1].0, "2.0.0 carries the wider context: {digests:?}");
    assert_ne!(digests[0].1, digests[1].1);

    let gateway = Gateway { recipe: at("9.9.9"), ..Gateway::priced() };
    assert_eq!(prepare(&path, asked("chapter", false), destination("modal-prof-clean-recipe"), &gateway, &settled)
        .unwrap_err(), "cloud_clean_unsupported_preprocessing");
    assert_eq!(gateway.priced.load(Ordering::SeqCst), 0, "refused before the price is read");
}

/// **The estimate is clipped where the render is.** A hole at the page's
/// right or bottom edge grows by the isolation radius only as far as the
/// page, and the crop the plan binds is the crop the render cuts, under
/// either version: a 26 px hole at the right edge was estimated 304 px wide
/// and sent at 288.
#[test]
fn a_region_at_the_page_edge_is_estimated_at_the_crop_it_is_sent_at() {
    let path = chapter("edge-crop", 1, StripMode::Single);
    let ids = [Rect::new(230, 100, 26, 20), Rect::new(100, 230, 20, 26), Rect::new(0, 0, 26, 26)]
        .map(|bbox| detect(&path, 0, bbox, REMOTE_PICK, 0));
    for version in ["1.0.0", cleaner_core::engines::render::PREPROCESSING_VERSION] {
        let recipe = RenderRecipe { preprocessing_version: version.into(), ..recipe() };
        let bound = bind(&path, &ids, recipe_preprocessing(&recipe).unwrap()).unwrap();
        assert!(bound.too_large.is_empty());
        for (region, id) in bound.regions.iter().zip(&ids) {
            let sent = read_render_input(&path, 0, id, &recipe).unwrap().crop_bounds;
            assert_eq!(region.crop, sent, "{version} {id}");
        }
        let gateway = Gateway { recipe, ..Gateway::priced() };
        let proposal = prepare(&path, asked("chapter", false), destination("modal-prof-clean-edge"), &gateway,
            &settled).unwrap();
        let sent: u64 = bound.regions.iter().map(|region| u64::from(region.crop.w) * u64::from(region.crop.h)).sum();
        assert_eq!(proposal.total_crop_pixels, sent, "{version}");
    }
}

/// **A region the render service cannot take is not planned.** Its crop is
/// past the service's limits even with no page around it, so it is held out
/// of the plan, named in `tooLargeIds`, before any price, plan digest or
/// consent; the rest of the scope goes on. A scope of only such regions is
/// no proposal at all.
#[test]
fn a_region_too_large_to_send_is_held_out_before_consent() {
    let path = chapter_of("too-large", 1, StripMode::Single, 2200, 64);
    let big = detect(&path, 0, Rect::new(20, 10, 2044, 30), REMOTE_PICK, 0);
    let small = detect(&path, 0, Rect::new(100, 20, 30, 20), REMOTE_PICK, 1);
    let gateway = Gateway { recipe: RenderRecipe {
        preprocessing_version: cleaner_core::engines::render::PREPROCESSING_VERSION.into(), ..recipe()
    }, ..Gateway::priced() };

    let proposal = prepare(&path, asked("chapter", false), destination("modal-prof-clean-large"), &gateway,
        &settled).unwrap();
    assert_eq!(proposal.region_ids, vec![small.clone()]);
    assert_eq!(proposal.too_large_ids, vec![big.clone()]);
    assert!(proposal.unresolved_ids.is_empty());
    let sent = read_render_input(&path, 0, &small, &gateway.recipe).unwrap().crop_bounds;
    assert_eq!(proposal.total_crop_pixels, u64::from(sent.w) * u64::from(sent.h), "the big one is not priced");

    let only = PrepareRequest {
    qwen_edit: None, scope: "regions".into(), region_ids: Some(vec![big.clone()]), ..asked("regions", false) };
    let priced = gateway.priced.load(Ordering::SeqCst);
    let none = prepare(&path, only, destination("modal-prof-clean-large"), &gateway, &settled).unwrap();
    assert_eq!((none.proposal_id, none.regions, none.plan_digest), (None, 0, None));
    assert_eq!(none.too_large_ids, vec![big]);
    assert_eq!(gateway.priced.load(Ordering::SeqCst), priced, "nothing to send is not priced");
}

/// The same number of jobs costs more when their crops are larger: the
/// estimate is each job's own crop at the size the worker edits it.
#[test]
fn the_estimate_follows_each_jobs_crop_not_a_count() {
    // Smaller crops are upscaled to a 768 px long side, snapped down to the
    // stride; larger ones are edited at their own size.
    assert_eq!(working_size(96, 48), (768, 384));
    assert_eq!(working_size(100, 60), (768, 448));
    assert_eq!(working_size(1024, 512), (1024, 512));
    assert_eq!(working_size(16, 16), (768, 768));

    let region = |w: u32, h: u32| GrantedRegion {
        id: format!("{w}x{h}"), page_index: 0, order: 0, bbox: Rect::new(0, 0, w, h), space: 0, revision_hash: String::new(),
        crop: cleaner_core::engines::render::crop_for(Rect::new(0, 0, w, h)).unwrap(), group_id: None,
        local_candidate: false,
    };
    let small = Estimate::of(&[region(20, 10), region(20, 10)], RenderRate::of(&recipe()));
    let large = Estimate::of(&[region(900, 700), region(900, 700)], RenderRate::of(&recipe()));
    assert!(large.gpu_seconds > small.gpu_seconds, "{small:?} {large:?}");
    let crop = cleaner_core::engines::render::crop_for(Rect::new(0, 0, 20, 10)).unwrap();
    assert_eq!(small.crop_pixels, 2 * u64::from(crop.w) * u64::from(crop.h));
    let (w, h) = working_size(crop.w, crop.h);
    let per_job = JOB_OVERHEAD_SECONDS + f64::from(w * h) / 1e6 * SECONDS_PER_WORK_MEGAPIXEL;
    assert!((small.gpu_seconds - 2.0 * per_job).abs() < 1e-9);
    assert_eq!(small.seconds().high, small.gpu_seconds + COLD_START_SECONDS + IDLE_WINDOW_SECONDS);

    assert_eq!(IDLE_WINDOW_SECONDS, 600.0, "high covers the deployment's maximum idle setting");
    // FLUX 4B holds two pipeline copies (deploy/cloud/common/capacity.py):
    // 12 + 7 GiB in one Modal process, two 12 GiB Beam workers.
    let per_second = |memory: f64| 3.6 / 3600.0 + CPU_CORES * CPU_USD_PER_CORE_SECOND + memory * MEMORY_USD_PER_GIB_SECOND;
    let priced = cost(80.0, 3.6, &recipe(), CloudProvider::Modal);
    assert!((priced.low - 80.0 * per_second(19.0)).abs() < 1e-12);
    assert!((priced.high - (80.0 + COLD_START_SECONDS + IDLE_WINDOW_SECONDS) * per_second(19.0)).abs() < 1e-12);
    let beam = cost(80.0, 3.6, &recipe(), CloudProvider::Beam);
    assert!((beam.low - 80.0 * per_second(24.0)).abs() < 1e-12);
    let nine = RenderRecipe { model_id: "Disty0/FLUX.2-klein-9B-SDNQ".into(), ..recipe() };
    assert_eq!(RenderRate::of(&nine).memory_gib(CloudProvider::Modal), 48.0, "the 9B holds two copies on the Modal L40S");
    assert_eq!(RenderRate::of(&nine).memory_gib(CloudProvider::Beam), 24.0, "and one on the Beam RTX5090");
    assert!(cost(80.0, 3.6, &nine, CloudProvider::Modal).low > priced.low, "the 9B worker has more memory");
}

/// Qwen edits every crop at about one megapixel on a 32 px grid, whatever its
/// size, and its worker has more memory and a longer cold start.
#[test]
fn the_qwen_estimate_uses_its_own_grid_and_rates() {
    let qwen = RenderRecipe { model_id: "Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32".into(), ..recipe() };
    let rate = RenderRate::of(&qwen);
    assert_eq!(rate.working_size(64, 64), (1024, 1024));
    assert_eq!(rate.working_size(320, 848), (640, 1664));
    assert_eq!(rate.working_size(1080, 1536), (864, 1216));
    assert_eq!(RenderRate::of(&recipe()).working_size(96, 48), working_size(96, 48));
    let region = GrantedRegion {
        id: "r".into(), page_index: 0, order: 0, bbox: Rect::new(0, 0, 20, 10), space: 0, revision_hash: String::new(),
        crop: cleaner_core::engines::render::crop_for(Rect::new(0, 0, 20, 10)).unwrap(), group_id: None,
        local_candidate: false,
    };
    let estimate = Estimate::of(std::slice::from_ref(&region), rate);
    assert!((estimate.seconds().high - estimate.gpu_seconds - estimate.retry_gpu_seconds - 90.0 - IDLE_WINDOW_SECONDS).abs() < 1e-9);
    assert!(estimate.retry_gpu_seconds > 0.0);
    assert!(cost(80.0, 3.6, &qwen, CloudProvider::Modal).low > cost(80.0, 3.6, &recipe(), CloudProvider::Modal).low,
        "the Qwen worker has more memory");
}

#[test]
fn confirmed_grant_capacity_refuses_a_seventeenth_live_grant() {
    let path = chapter("grant-capacity", 1, StripMode::Single);
    let id = detect(&path, 0, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let grants = GrantService::new();
    let scope = scope_for(&path, &[id], &grants);
    let mut store = HashMap::new();
    for n in 0..MAX_OPEN_GRANTS {
        assert!(grant_has_capacity(&store));
        store.insert(n.to_string(), StoredGrant { scope: scope.clone(), expires_at_ms: u64::MAX });
    }
    assert!(!grant_has_capacity(&store));
}

/// A detection's text group is part of what consent binds, so the plan names
/// the group each request cleans.
#[test]
fn the_plan_carries_each_regions_text_group() {
    let path = chapter("plan-group", 1, StripMode::Single);
    let bbox = Rect::new(40, 40, 30, 20);
    let id = detect(&path, 0, bbox, REMOTE_PICK, 0);
    {
        let mut job = Job::open(&path).unwrap();
        let mut row = job.project.detections[0].clone();
        row.group = Some(group("tg-00000000beef", bbox));
        job.store_detection(row, &Mask::filled(bbox), &Mask::filled(bbox)).unwrap();
    }
    let scope = scope_for(&path, std::slice::from_ref(&id), &GrantService::new());
    assert_eq!(scope.regions[0].group_id.as_deref(), Some("tg-00000000beef"));
    let mut regrouped = scope.clone();
    regrouped.regions[0].group_id = Some("tg-00000000cafe".into());
    assert_ne!(regrouped.digest().unwrap(), scope.plan_digest);
}

/* ------------------------------------------------------------------ */
/* Consent and grant                                                   */
/* ------------------------------------------------------------------ */

fn proposed(path: &Path, profile: &str) -> String {
    prepare(path, asked("chapter", false), destination(profile), &Gateway::priced(), &settled).unwrap()
        .proposal_id.unwrap()
}

/// The plan digest a proposal's consent dialog shows; empty once it is gone.
fn shown(proposal_id: &str) -> String {
    proposals().lock().unwrap().get(proposal_id).map(|stored| stored.scope.plan_digest.clone()).unwrap_or_default()
}

/// Confirm with the plan the dialog showed, as the interface does.
fn agree(proposal_id: &str, rights: bool, retention: bool) -> Result<CloudCleanGrant, String> {
    confirm(proposal_id, &shown(proposal_id), rights, retention)
}

fn request<'a>(profile: &'a str, recipe: &'a RenderRecipe) -> CleanGrantRequest<'a> {
    CleanGrantRequest {
        provider: CloudProvider::Modal, profile_id: profile, endpoint_fingerprint: &FINGERPRINT,
        profile_epoch: GrantService::global().get_profile_epoch(CloudProvider::Modal, profile), recipe,
    }
}

#[test]
fn consent_answers_are_required_and_a_proposal_is_used_once() {
    let profile = "modal-prof-clean-consent";
    let path = chapter("consent", 1, StripMode::Single);
    detect(&path, 0, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let id = proposed(&path, profile);
    assert_eq!(agree(&id, false, true).unwrap_err(), "rights_attestation_required");
    assert_eq!(agree(&id, true, false).unwrap_err(), "retention_acknowledgement_required");
    let grant = agree(&id, true, true).unwrap();
    assert!(grant.expires_at_ms > analysis::now_ms());
    assert_eq!(agree(&id, true, true).unwrap_err(), "cloud_clean_proposal_missing");

    let stale = proposed(&path, profile);
    proposals().lock().unwrap().get_mut(&stale).unwrap().expires_at_ms = 0;
    assert!(matches!(agree(&stale, true, true).unwrap_err().as_str(),
        "cloud_clean_proposal_expired" | "cloud_clean_proposal_missing"),
        "another prepare may evict the expired entry before confirm");

    let dropped = proposed(&path, profile);
    assert!(discard(&dropped).unwrap());
    assert!(!discard(&dropped).unwrap());
    assert_eq!(agree(&dropped, true, true).unwrap_err(), "cloud_clean_proposal_missing");

    // Consent is given for the plan the dialog showed. A digest that is not
    // this proposal's (another plan's, or none) mints nothing, and spends it.
    let first = proposed(&path, profile);
    let second = proposed(&path, profile);
    let (first_plan, second_plan) = (shown(&first), shown(&second));
    assert_eq!(first_plan, second_plan, "the same regions make the same plan");
    let other = chapter("consent-other", 1, StripMode::Single);
    detect(&other, 0, Rect::new(60, 60, 30, 20), REMOTE_PICK, 0);
    let elsewhere = proposed(&other, profile);
    assert_ne!(shown(&elsewhere), first_plan);
    assert_eq!(confirm(&first, &shown(&elsewhere), true, true).unwrap_err(), "cloud_clean_plan_mismatch");
    assert_eq!(confirm(&first, &first_plan, true, true).unwrap_err(), "cloud_clean_proposal_missing",
        "a refused plan is spent");
    assert_eq!(confirm(&second, "", true, true).unwrap_err(), "cloud_clean_plan_mismatch");
    let mut flipped = shown(&elsewhere).into_bytes();
    flipped[0] = if flipped[0] == b'0' { b'1' } else { b'0' };
    assert_eq!(confirm(&elsewhere, &String::from_utf8(flipped).unwrap(), true, true).unwrap_err(),
        "cloud_clean_plan_mismatch");
    assert!(proposals().lock().unwrap().get(&elsewhere).is_none());

    // Its own profile id: the epoch is process-wide.
    let edited = "modal-prof-clean-consent-edited";
    let id = proposed(&path, edited);
    GrantService::global().invalidate_profile(CloudProvider::Modal, edited);
    assert_eq!(agree(&id, true, true).unwrap_err(), "cloud_clean_profile_changed");
}

#[test]
fn a_standing_project_confirms_without_the_statements_only_when_it_holds_them() {
    let profile = "modal-prof-clean-standing";
    let path = chapter("standing", 1, StripMode::Single);
    detect(&path, 0, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let scratch = std::env::temp_dir().join(format!("mc-cloud-clean-standing-lib-{}-{}", std::process::id(),
        analysis::now_ms()));
    let library = Library::at(&scratch);
    let project = library.create_project("One", StripMode::Single, None, None).unwrap();
    let chapter_id = library.create_chapter(&project.id, "A", None, None).unwrap().created().unwrap().id;
    let propose = || {
        let request = PrepareRequest {
        qwen_edit: None, chapter_id: chapter_id.clone(), ..asked("chapter", false) };
        prepare(&path, request, destination(profile), &Gateway::priced(), &settled).unwrap().proposal_id.unwrap()
    };
    let confirm = |id: &str, statements| confirm_in_project(Some(&library), id, &shown(id), statements, statements);
    // A refused answer leaves its proposal open; drop it, as a cancel does.
    let refused = |statements| {
        let id = propose();
        let error = confirm(&id, statements).unwrap_err();
        assert!(discard(&id).unwrap());
        error
    };

    assert_eq!(refused(false), "rights_attestation_required");
    // A consent without the statements, as recorded before they were asked.
    library.set_cloud_consent(&chapter_id, Some(library::CloudConsent {
        provider: CloudProvider::Modal, profile_id: profile.into(), origin_fingerprint: FINGERPRINT.clone(),
        granted_at: 0, statements_accepted: false,
    })).unwrap();
    assert_eq!(refused(false), "rights_attestation_required");
    confirm(&propose(), true).unwrap();
    assert!(library.cloud_consent_covers(&chapter_id, CloudProvider::Modal, profile, &FINGERPRINT));
    confirm(&propose(), false).expect("the project's statements stand for it");
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_grant_binds_the_plan_destination_and_recipe_and_is_single_use() {
    let profile = "modal-prof-clean-grant";
    let path = chapter("grant", 2, StripMode::Single);
    let first = detect(&path, 0, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let second = detect(&path, 1, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let current = recipe();
    let mint = || agree(&proposed(&path, profile), true, true).unwrap().grant_id;

    let ok = mint();
    let scope = consume_grant(&ok, &request(profile, &current)).unwrap();
    assert_eq!(scope.chapter_id, "chapter");
    assert_eq!(scope.regions.iter().map(|region| region.id.clone()).collect::<Vec<_>>(), vec![first, second]);
    assert_eq!(scope.regions.iter().map(|region| region.page_index).collect::<Vec<_>>(), vec![0, 1]);
    assert!(scope.regions.iter().all(|region| region.revision_hash.len() == 64));
    assert_eq!((scope.recipe.clone(), scope.chunk_regions, scope.execution), (current.clone(), CHUNK_REGIONS,
        CleanExecution::Cloud));
    assert_eq!(scope.digest().unwrap(), scope.plan_digest);
    assert_eq!(consume_grant(&ok, &request(profile, &current)).unwrap_err(), "cloud_clean_grant_missing");

    let other_profile = mint();
    let mut presented = request(profile, &current);
    presented.profile_id = "modal-prof-clean-other";
    assert_eq!(consume_grant(&other_profile, &presented).unwrap_err(), "cloud_clean_grant_mismatch: profile");
    // Refused once is spent: the right arguments cannot follow.
    assert_eq!(consume_grant(&other_profile, &request(profile, &current)).unwrap_err(), "cloud_clean_grant_missing");

    let other_provider = mint();
    let mut presented = request(profile, &current);
    presented.provider = CloudProvider::Beam;
    assert_eq!(consume_grant(&other_provider, &presented).unwrap_err(), "cloud_clean_grant_mismatch: profile");

    let other_endpoint = mint();
    let moved = "0".repeat(64);
    let mut presented = request(profile, &current);
    presented.endpoint_fingerprint = &moved;
    assert_eq!(consume_grant(&other_endpoint, &presented).unwrap_err(), "cloud_clean_grant_mismatch: endpoint");

    let other_recipe = mint();
    let redeployed = RenderRecipe { model_revision: "f".repeat(40), ..recipe() };
    assert_eq!(consume_grant(&other_recipe, &request(profile, &redeployed)).unwrap_err(),
        "cloud_clean_grant_mismatch: recipe");

    let stale_epoch = mint();
    let mut presented = request(profile, &current);
    presented.profile_epoch += 1;
    assert_eq!(consume_grant(&stale_epoch, &presented).unwrap_err(), "cloud_clean_profile_changed");

    let expired = mint();
    grants().lock().unwrap().get_mut(&expired).unwrap().expires_at_ms = 0;
    assert_eq!(consume_grant(&expired, &request(profile, &current)).unwrap_err(), "cloud_clean_grant_expired");

    assert_eq!(consume_grant("never-issued", &request(profile, &current)).unwrap_err(), "cloud_clean_grant_missing");
}

/// Each term the consent showed is bound: the order, the chunking rule, the
/// estimate and where the regions are cleaned.
#[test]
fn the_plan_digest_binds_order_chunking_estimate_and_execution() {
    let path = chapter("digest", 1, StripMode::Single);
    let ids = vec![
        detect(&path, 0, Rect::new(20, 20, 30, 20), REMOTE_PICK, 0),
        detect(&path, 0, Rect::new(120, 120, 30, 20), REMOTE_PICK, 1),
    ];
    let scope = scope_for(&path, &ids, &GrantService::new());
    let changed = |change: fn(&mut CleanGrantScope)| {
        let mut other = scope.clone();
        change(&mut other);
        other.digest().unwrap()
    };
    assert_eq!(scope.digest().unwrap(), scope.plan_digest);
    for digest in [
        changed(|scope| scope.regions.swap(0, 1)),
        changed(|scope| { scope.regions.pop(); }),
        changed(|scope| scope.chunk_regions = 1),
        changed(|scope| scope.estimate.work_pixels += 1),
        changed(|scope| scope.cost = None),
        changed(|scope| scope.price = Some(("A100".into(), 2.5))),
        changed(|scope| scope.execution = CleanExecution::Mixed),
        changed(|scope| scope.regions[1].revision_hash = "0".repeat(64)),
    ] {
        assert_ne!(digest, scope.plan_digest);
    }
}

/* ------------------------------------------------------------------ */
/* The run                                                             */
/* ------------------------------------------------------------------ */

/// A render that commits a flat patch through the one commit a cloud render
/// uses, and records how it was called.
struct Renderer {
    job_path: PathBuf,
    grants: Arc<GrantService>,
    delay: Duration,
    /// Region ids that fail at once, with their code.
    fail: HashMap<String, String>,
    /// While set, a render waits until it is cancelled.
    hold: bool,
    /// Called with each region id as its render starts.
    on_render: Box<dyn Fn(&str) + Sync>,
    /// Without `commit`, a render answers its region from here and writes
    /// nothing: a transport that only counts.
    answers: HashMap<String, ApiRegion>,
    commit: bool,
    /// Use the real input/grant check for fixtures backed by real crops.
    validate_render_grant: bool,
    /// What the deployment answers at each read, the last one repeating,
    /// and a hook called with the count of reads so far (1 before the first
    /// chunk).
    deployed: Vec<(RenderRecipe, Option<(String, f64)>)>,
    on_deployment: Box<dyn Fn(usize) + Sync>,
    /// How many renders had started at each deployment read.
    rendered_at_deployment: Mutex<Vec<usize>>,
    pages_in_flight: Mutex<Vec<u32>>,
    most_in_flight: AtomicUsize,
    same_page_at_once: AtomicBool,
    rendered: Mutex<Vec<String>>,
    cancelled: Mutex<Vec<String>>,
    releases: AtomicUsize,
}

impl Renderer {
    fn new(job_path: &Path, grants: &Arc<GrantService>) -> Self {
        Self {
            job_path: job_path.to_path_buf(), grants: Arc::clone(grants), delay: Duration::from_millis(40),
            fail: HashMap::new(), hold: false, on_render: Box::new(|_| {}), answers: HashMap::new(), commit: true,
            validate_render_grant: false,
            deployed: vec![(recipe(), l4())], on_deployment: Box::new(|_| {}),
            rendered_at_deployment: Mutex::default(),
            pages_in_flight: Mutex::default(), most_in_flight: AtomicUsize::new(0),
            same_page_at_once: AtomicBool::new(false), rendered: Mutex::default(), cancelled: Mutex::default(),
            releases: AtomicUsize::new(0),
        }
    }

    fn rendered(&self) -> Vec<String> { self.rendered.lock().unwrap().clone() }
}

impl RegionRenderer for Renderer {
    fn render(&self, nonce: &str, page_index: u32, region_id: &str, recipe: &RenderRecipe) -> Result<ApiRegion, String> {
        // Minted for this one region, against the run's own grant service.
        let grant = self.grants.get_grant(nonce).expect("a region grant");
        assert_eq!(grant.scope.region_ids, vec![region_id.to_owned()]);
        assert_eq!(grant.max_attempts, 1);
        assert_eq!(&grant.scope.recipe, recipe, "render must use the consented recipe");
        if self.validate_render_grant {
            let input = read_render_input(&self.job_path, page_index, region_id, recipe).unwrap();
            let requested_scope = input.scope(grant.scope.provider, &grant.scope.profile_id,
                &grant.scope.canonical_endpoint_fingerprint,
                compute_operation_digest(&OperationIntent::CloudClean).unwrap(), recipe, region_id);
            self.grants.validate_and_consume(nonce, &requested_scope).expect("render must match the full grant scope");
        }
        self.rendered.lock().unwrap().push(region_id.to_owned());
        (self.on_render)(region_id);
        if let Some(code) = self.fail.get(region_id) { return Err(code.clone()); }
        if !self.commit {
            return Ok(self.answers.get(region_id).expect("an answer for every planned region").clone());
        }
        {
            let mut pages = self.pages_in_flight.lock().unwrap();
            if pages.contains(&page_index) { self.same_page_at_once.store(true, Ordering::SeqCst); }
            pages.push(page_index);
            self.most_in_flight.fetch_max(pages.len(), Ordering::SeqCst);
        }
        let attempt = attempt_id_for_nonce(nonce);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        std::thread::sleep(self.delay);
        while self.hold && !self.cancelled.lock().unwrap().contains(&attempt) {
            assert!(std::time::Instant::now() < deadline, "a held render was never cancelled");
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut pages = self.pages_in_flight.lock().unwrap();
        let at = pages.iter().position(|page| *page == page_index).unwrap();
        pages.remove(at);
        drop(pages);
        if self.cancelled.lock().unwrap().contains(&attempt) { return Err("cancelled".into()); }

        let _lock = run::lock_job(&self.job_path).unwrap();
        let mut job = Job::open(&self.job_path).unwrap();
        let loaded = job.load_detection(region_id).unwrap().expect("the detection is still there");
        let bounds = loaded.mask.bounds;
        let pixels = Raster { width: bounds.w, height: bounds.h, data: vec![200; (bounds.w * bounds.h) as usize],
            ..page_raster() };
        let patch = Patch {
            id: region_id.to_owned(), mask: loaded.mask, ink: loaded.ink, pixels, order: loaded.record.order,
            visible: true,
            provenance: Provenance {
                engine: Engine::Flux, engine_version: "test".into(), model_sha256: None,
                execution_provider: "cloud".into(), params_snapshot: serde_json::json!({}),
                mask_sha256: String::new(), source_sha256: String::new(), cloud: None, created: 1_760_000_000,
            },
        };
        assert!(job.complete_detection(region_id, &patch, None).unwrap());
        library::invalidate_manifest_cache(&self.job_path);
        let page = library::page_of("chapter", &job.project, page_index as usize).unwrap();
        Ok(page.regions.into_iter().find(|region| region.id == region_id).unwrap())
    }

    fn deployment(&self) -> Result<(RenderRecipe, Option<(String, f64)>), String> {
        let reads = {
            let mut at = self.rendered_at_deployment.lock().unwrap();
            at.push(self.rendered.lock().unwrap().len());
            at.len()
        };
        (self.on_deployment)(reads);
        Ok(self.deployed[(reads - 1).min(self.deployed.len() - 1)].clone())
    }

    fn cancel(&self, attempt_id: &str) {
        let mut cancelled = self.cancelled.lock().unwrap();
        if !cancelled.iter().any(|id| id == attempt_id) { cancelled.push(attempt_id.to_owned()); }
    }

    fn release_idle(&self) { self.releases.fetch_add(1, Ordering::SeqCst); }
}

/// The scope a spent grant would hold for these ids, against `grants`, cut
/// into chunks of `chunk_regions`.
fn plan_for(path: &Path, ids: &[String], grants: &GrantService, chunk_regions: usize, execution: CleanExecution)
    -> CleanGrantScope {
    let Bound { regions, too_large } = bind(path, ids, recipe_preprocessing(&recipe()).unwrap()).unwrap();
    assert!(too_large.is_empty());
    let estimate = Estimate::of(&regions, RenderRate::of(&recipe()));
    let mut scope = CleanGrantScope {
        chapter_id: "chapter".into(),
        regions,
        provider: CloudProvider::Modal,
        profile_id: "modal-prof-clean-run".into(),
        endpoint_fingerprint: FINGERPRINT.clone(),
        profile_epoch: grants.get_profile_epoch(CloudProvider::Modal, "modal-prof-clean-run"),
        recipe: recipe(),
        execution,
        chunk_regions,
        estimate,
        price: l4(),
        cost: Some(cost(estimate.gpu_seconds, 0.8, &recipe(), CloudProvider::Modal)),
        plan_digest: String::new(),
    };
    scope.plan_digest = scope.digest().unwrap();
    scope
}

fn scope_for(path: &Path, ids: &[String], grants: &GrantService) -> CleanGrantScope {
    plan_for(path, ids, grants, CHUNK_REGIONS, CleanExecution::Cloud)
}

/// Everything a run is handed besides its plan.
struct Harness<'a> {
    allowed: &'a (dyn Fn() -> Result<(), &'static str> + Sync),
    unresolved: &'a Unresolved<'a>,
    local_pass: Option<&'a LocalPass<'a>>,
    read: &'a ReadInput<'a>,
}

const OPEN: Harness<'static> = Harness {
    allowed: &|| Ok(()), unresolved: &settled, local_pass: None, read: &read_render_input,
};

/// Run a plan through a renderer, answering the summary and every event.
fn run_scope(scope: &CleanGrantScope, path: &Path, grants: &Arc<GrantService>, renderer: &Renderer,
    cancel: &AtomicBool, harness: &Harness<'_>) -> (Summary, Vec<Event>) {
    let batch = Batch {
        scope, job_path: path, grants, allowed: harness.allowed, unresolved: harness.unresolved,
        local_pass: harness.local_pass, read: harness.read,
    };
    let events = RefCell::new(Vec::new());
    let summary = run_plan(&batch, renderer, "run-test", cancel, &|event| events.borrow_mut().push(event));
    (summary, events.into_inner())
}

/// Run `ids` as one chunk through a renderer.
fn run_with(path: &Path, ids: &[String], grants: &Arc<GrantService>, renderer: &Renderer, cancel: &AtomicBool)
    -> (Summary, Vec<Event>) {
    run_scope(&scope_for(path, ids, grants), path, grants, renderer, cancel, &OPEN)
}

fn of_type(events: &[Event], kind: &str) -> usize {
    events.iter().filter(|event| serde_json::to_value(event).unwrap()["type"] == kind).count()
}

fn notices<'a>(events: &'a [Event], wanted: &str) -> Vec<&'a serde_json::Value> {
    events.iter().filter_map(|event| match event {
        Event::Notice { key, params, .. } if key == wanted => Some(params),
        _ => None,
    }).collect()
}

/// A gateway advertises deployment identity without request-local guidance.
/// Batch renders must still spend the exact recipe consented to in the plan.
#[test]
fn batch_renders_keep_qwen_guidance_bound_to_their_region_grants() {
    use cleaner_core::engines::render::{QwenEdit, QwenTarget};
    let qwen = RenderRecipe::new("mc-qwen-image-edit-2511-v4", "1.0.0",
        "Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32", recipe().model_revision, false);
    let guided = QwenEdit { target: QwenTarget::SoundEffect, description: "Letters beside the hand".into() };
    for (name, deployment, guidance, expected) in [
        ("guided", qwen.clone(), Some(guided.clone()), Some(guided)),
        ("default", qwen, None, Some(QwenEdit { target: QwenTarget::Auto, description: String::new() })),
        ("flux", recipe(), None, None),
    ] {
        let profile = format!("modal-prof-batch-guidance-{name}");
        let path = chapter(&format!("batch-guidance-{name}"), 1, StripMode::Single);
        let id = detect(&path, 0, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
        let gateway = Gateway { recipe: deployment.clone(), ..Gateway::priced() };
        let proposal = prepare(&path, PrepareRequest { qwen_edit: guidance, ..asked("chapter", false) },
            destination(&profile), &gateway, &settled).unwrap();
        let grant = agree(proposal.proposal_id.as_deref().unwrap(), true, true).unwrap();
        let scope = consume_grant(&grant.grant_id, &request(&profile, &deployment)).unwrap();
        assert_eq!(scope.recipe.qwen_edit, expected);
        assert_eq!(scope.plan_digest, scope.digest().unwrap());
        let grants = Arc::new(GrantService::new());
        let mut renderer = Renderer::new(&path, &grants);
        renderer.deployed = vec![(deployment, l4())];
        renderer.validate_render_grant = true;
        let (summary, _) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false), &OPEN);
        // Renderer validates and consumes the per-region grant using the
        // recipe actually supplied by the batch, before committing a patch.
        assert_eq!(summary.regions_cleaned, 1, "{name}");
        assert_eq!(renderer.rendered(), vec![id]);
        assert!(detection_ids(&path).is_empty());
    }
}

/// Two regions on each of four pages: three renders at once, never two of one
/// page, every region committed and the GPU released once at the end.
#[test]
fn three_renders_in_flight_on_different_pages_and_one_release() {
    let path = chapter("run-bound", 4, StripMode::Single);
    let ids: Vec<String> = (0..4).flat_map(|page| [
        detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0),
        detect(&path, page, Rect::new(150, 150, 30, 20), REMOTE_PICK, 1),
    ]).collect();
    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    // Make the first three wait for one another at dispatch. A wall clock
    // delay is not enough under the load of the full parallel test suite.
    let first_three = Arc::new((Mutex::new(0usize), Condvar::new()));
    renderer.on_render = Box::new(move |_| {
        let (count, changed) = &*first_three;
        let mut count = count.lock().unwrap();
        *count += 1;
        if *count >= MAX_IN_FLIGHT {
            changed.notify_all();
        } else {
            let (seen, _) = changed.wait_timeout_while(count, Duration::from_secs(10),
                |seen| *seen < MAX_IN_FLIGHT).unwrap();
            assert!(*seen >= MAX_IN_FLIGHT, "three workers did not reach the renderer");
        }
    });
    renderer.delay = Duration::from_millis(150);
    let (summary, events) = run_with(&path, &ids, &grants, &renderer, &AtomicBool::new(false));

    assert_eq!(renderer.most_in_flight.load(Ordering::SeqCst), MAX_IN_FLIGHT);
    assert!(!renderer.same_page_at_once.load(Ordering::SeqCst), "two regions of one page read each other");
    assert_eq!((summary.reason, summary.regions_cleaned, summary.pages_cleaned, summary.pages_queued),
        ("completed", 8, 4, 4));
    assert_eq!((of_type(&events, "page-started"), of_type(&events, "region-done"), of_type(&events, "page-done")),
        (4, 8, 4));
    assert!(detection_ids(&path).is_empty());
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 1);
}

/// A failed region stays detected with a notice and the run goes on; a region
/// edited or deleted since consent is skipped with a notice and never sent.
#[test]
fn a_failure_leaves_its_detection_and_changed_or_deleted_regions_are_skipped() {
    let path = chapter("run-failure", 4, StripMode::Single);
    let ids: Vec<String> = (0..4).map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
    let grants = Arc::new(GrantService::new());
    let scope = scope_for(&path, &ids, &grants);
    // After consent: page 2's detection is refitted, page 3's deleted.
    {
        let mut job = Job::open(&path).unwrap();
        let bigger = Rect::new(40, 40, 34, 20);
        job.store_detection(record(ids[2].clone(), 2, bigger, REMOTE_PICK, 0), &Mask::filled(bigger), &Mask::filled(bigger))
            .unwrap();
        job.remove_detection(&ids[3]).unwrap();
    }
    let mut renderer = Renderer::new(&path, &grants);
    renderer.fail.insert(ids[1].clone(), "remote_failed".into());
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false), &OPEN);

    let mut rendered = renderer.rendered();
    rendered.sort();
    assert_eq!(rendered, vec![ids[0].clone(), ids[1].clone()], "a changed or deleted region was sent");
    let failed = notices(&events, "notice.cloudClean.regionFailed");
    assert_eq!(failed.len(), 1);
    assert_eq!((failed[0]["regionId"].as_str(), failed[0]["page"].as_u64(), failed[0]["code"].as_str()),
        (Some(ids[1].as_str()), Some(2), Some("remote_failed")));
    let mut skipped: Vec<(&str, &str)> = notices(&events, "notice.cloudClean.regionSkipped").iter()
        .map(|params| (params["regionId"].as_str().unwrap(), params["reason"].as_str().unwrap())).collect();
    // Three workers: the notices come in the order the regions finish.
    skipped.sort();
    assert_eq!(skipped, vec![(ids[2].as_str(), "changed"), (ids[3].as_str(), "gone")]);
    assert_eq!(detection_ids(&path), vec![ids[1].clone(), ids[2].clone()]);
    assert_eq!((summary.reason, summary.regions_cleaned, summary.errored), ("completed", 1, 1));
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 1);
}

/// A refused key stops the run: the renders in flight are cancelled, nothing
/// more is started, and a run that no longer speaks for the endpoint does not
/// ask it for anything more.
#[test]
fn a_refused_key_stops_the_run_and_cancels_what_is_in_flight() {
    let path = chapter("run-refused", 5, StripMode::Single);
    let ids: Vec<String> = (0..5).map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    renderer.fail.insert(ids[0].clone(), "gateway_unauthorized".into());
    renderer.hold = true;
    let (summary, events) = run_with(&path, &ids, &grants, &renderer, &AtomicBool::new(false));

    let stopped = notices(&events, "notice.cloudClean.stopped");
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["code"], "gateway_unauthorized");
    assert!(notices(&events, "notice.cloudClean.regionFailed").is_empty());
    let rendered = renderer.rendered();
    assert!(rendered.len() <= MAX_IN_FLIGHT, "started after the stop: {rendered:?}");
    assert!(!rendered.contains(&ids[3]) && !rendered.contains(&ids[4]));
    assert_eq!(renderer.cancelled.lock().unwrap().len(), rendered.len() - 1, "the others in flight were cancelled");
    assert_eq!(detection_ids(&path), ids, "nothing was committed");
    assert_eq!((summary.reason, summary.regions_cleaned), ("completed", 0));
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 0);
    assert_eq!(of_type(&events, "page-started"), of_type(&events, "page-done"));
}

/// An edit to the profile mid-run stops the next region at its grant.
#[test]
fn a_changed_profile_epoch_stops_the_next_region_before_it_is_sent() {
    let path = chapter("run-epoch", 1, StripMode::Single);
    // One page, so the three go one after another.
    let ids: Vec<String> = (0..3).map(|n| detect(&path, 0, Rect::new(20 + 70 * n as i64, 40, 30, 20), REMOTE_PICK, n))
        .collect();
    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    let edits = Arc::clone(&grants);
    renderer.on_render = Box::new(move |_| {
        edits.invalidate_profile(CloudProvider::Modal, "modal-prof-clean-run");
    });
    let (summary, events) = run_with(&path, &ids, &grants, &renderer, &AtomicBool::new(false));

    assert_eq!(renderer.rendered(), vec![ids[0].clone()]);
    let stopped = notices(&events, "notice.cloudClean.stopped");
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["code"], "cloud_run_profile_changed");
    assert_eq!(summary.regions_cleaned, 1);
    assert_eq!(detection_ids(&path), vec![ids[1].clone(), ids[2].clone()]);
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 0);
}

/// `cancel_run`'s flag cancels the renders in flight and starts nothing more.
/// Every region is left detected, and the GPU the run used is released.
#[test]
fn cancel_stops_the_renders_in_flight_and_leaves_every_region_detected() {
    let path = chapter("run-cancel", 5, StripMode::Single);
    let ids: Vec<String> = (0..5).map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    renderer.hold = true;
    let cancel = AtomicBool::new(false);
    let (summary, events) = std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while renderer.rendered().len() < MAX_IN_FLIGHT {
                assert!(std::time::Instant::now() < deadline, "the renders never started");
                std::thread::sleep(Duration::from_millis(2));
            }
            cancel.store(true, Ordering::SeqCst);
        });
        run_with(&path, &ids, &grants, &renderer, &cancel)
    });

    assert_eq!(summary.reason, "cancelled");
    assert_eq!(summary.next_page_index, Some(0));
    assert_eq!(renderer.rendered().len(), MAX_IN_FLIGHT, "started after the cancel");
    assert_eq!(renderer.cancelled.lock().unwrap().len(), MAX_IN_FLIGHT);
    assert_eq!(detection_ids(&path), ids);
    assert!(notices(&events, "notice.cloudClean.regionFailed").is_empty());
    assert_eq!(of_type(&events, "page-started"), of_type(&events, "page-done"));
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 1);
}

#[test]
fn cancel_before_first_pick_sends_no_render() {
    let path = chapter("cancel-before-pick", 2, StripMode::Single);
    let ids: Vec<String> = (0..2).map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0))
        .collect();
    let grants = Arc::new(GrantService::new());
    let renderer = Renderer::new(&path, &grants);
    let (summary, events) = run_with(&path, &ids, &grants, &renderer, &AtomicBool::new(true));
    assert_eq!(summary.reason, "cancelled");
    assert!(renderer.rendered().is_empty());
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 0);
    assert_eq!(detection_ids(&path), ids);
    assert_eq!(of_type(&events, "page-started"), 0);
}

#[test]
fn a_region_waits_only_for_regions_below_it_within_reach_on_its_page() {
    let region = |page: u32, order: u32, x: i64| GrantedRegion {
        id: format!("{page}-{order}"), page_index: page, order, bbox: Rect::new(x, 0, 10, 10), space: page,
        revision_hash: String::new(), crop: Rect::new(x, 0, 10, 10), group_id: None, local_candidate: false,
    };
    // Out of reach of both regions below it.
    let far = DEPENDENCY_REACH + 60;
    let regions = [region(0, 0, 0), region(0, 1, 30), region(0, 2, far), region(1, 0, 0)];
    let mut slots = vec![Slot::InFlight, Slot::Queued, Slot::Queued, Slot::Queued];
    // Region 1 reads under region 0; region 2 is out of its reach.
    assert_eq!(next_ready(&slots, &regions), Some(2));
    slots[2] = Slot::InFlight;
    assert_eq!(next_ready(&slots, &regions), Some(3));
    slots[3] = Slot::InFlight;
    assert_eq!(next_ready(&slots, &regions), None);
    slots[0] = Slot::Done;
    assert_eq!(next_ready(&slots, &regions), Some(1));
    // A region of a later chunk is never picked before its chunk is minted.
    let waiting = vec![Slot::Done, Slot::Done, Slot::Waiting, Slot::Waiting];
    assert_eq!(next_ready(&waiting, &regions), None);
}

#[test]
fn on_a_long_strip_a_region_waits_across_a_join_in_either_direction() {
    // Strip pixels, one space. Orders are counted per page, so the region at
    // the top of page 1 (order 0) is below the one at the foot of page 0
    // (order 3) whatever their places in the plan.
    let region = |page: u32, order: u32, y: i64| GrantedRegion {
        id: format!("{page}-{order}"), page_index: page, order, bbox: Rect::new(0, y, 10, 10), space: 0,
        revision_hash: String::new(), crop: Rect::new(0, y, 10, 10), group_id: None, local_candidate: false,
    };
    let regions = [region(0, 3, 990), region(1, 0, 1005)];
    let mut slots = vec![Slot::Queued, Slot::Queued];
    assert_eq!(next_ready(&slots, &regions), Some(1));
    slots[1] = Slot::InFlight;
    assert_eq!(next_ready(&slots, &regions), None);
    slots[1] = Slot::Done;
    assert_eq!(next_ready(&slots, &regions), Some(0));
    // Far apart in the strip, neither waits.
    let apart = [region(0, 3, 0), region(1, 0, 5000)];
    assert_eq!(next_ready(&[Slot::Queued, Slot::Queued], &apart), Some(0));
    // A region in a chunk not yet minted is not waited on.
    assert_eq!(next_ready(&[Slot::Queued, Slot::Waiting], &regions), Some(0));
}

/* ------------------------------------------------------------------ */
/* Chunks under one consent                                            */
/* ------------------------------------------------------------------ */

/// Six regions, two to a page on three pages, far apart, cut into chunks of two.
fn three_chunks(name: &str) -> (PathBuf, Vec<String>) {
    let path = chapter(name, 3, StripMode::Single);
    let ids = (0..3).flat_map(|page| [
        detect(&path, page, Rect::new(10, 10, 20, 12), REMOTE_PICK, 0),
        detect(&path, page, Rect::new(200, 200, 20, 12), REMOTE_PICK, 1),
    ]).collect();
    (path, ids)
}

/// The acceptance case: 1,025 text groups, more than one chunk may carry,
/// run under the one plan consent was given for. Chunks of 256 go one after
/// another, every region exactly once, each through its own single-region
/// grant, and the deployment is checked again before each chunk.
#[test]
fn a_1025_group_plan_runs_in_bounded_chunks_under_one_consent() {
    const GROUPS: usize = MAX_CLEAN_REGIONS + 1;
    const PER_PAGE: usize = 25;
    let pages = GROUPS.div_ceil(PER_PAGE);
    let path = chapter("thousand", pages, StripMode::Single);
    let rows: Vec<(usize, Rect, u32)> = (0..GROUPS).map(|n| {
        let slot = (n % PER_PAGE) as i64;
        (n / PER_PAGE, Rect::new(8 + 48 * (slot % 5), 8 + 48 * (slot / 5), 12, 8), (n % PER_PAGE) as u32)
    }).collect();
    let ids = detect_many(&path, &rows);

    let profile = "modal-prof-clean-run";
    let gateway = Gateway::priced();
    let proposal = prepare(&path, asked("chapter", false), destination(profile), &gateway, &settled).unwrap();
    assert_eq!(proposal.region_ids, ids, "one plan, in page and reading order");
    assert_eq!((proposal.regions as usize, proposal.chunks, proposal.chunk_regions), (GROUPS, 5, 256));
    assert_eq!(gateway.asked.load(Ordering::SeqCst), 1, "one prepare, one consent");
    let grant = agree(proposal.proposal_id.as_deref().unwrap(), true, true).unwrap();
    let scope = consume_grant(&grant.grant_id, &request(profile, &recipe())).unwrap();
    assert_eq!(scope.plan_digest, proposal.plan_digest.unwrap());
    assert!(scope.regions.iter().all(|region| region.group_id.is_some()));

    // A transport that counts: every answer is the region as detected.
    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    renderer.commit = false;
    let job = Job::open(&path).unwrap();
    renderer.answers = (0..pages).flat_map(|page| library::page_of("chapter", &job.project, page).unwrap().regions)
        .map(|region| (region.id.clone(), region)).collect();
    let scope = CleanGrantScope {
        profile_epoch: grants.get_profile_epoch(CloudProvider::Modal, profile), ..scope
    };
    // Each region is read again as the run reaches it. Reading 1,025 from a
    // debug build's disk would take minutes, so the reread answers each
    // region at its bound revision, from one real read of the first; the
    // reread itself is the real one in every other run test.
    let template = read_render_input(&path, 0, &ids[0], &scope.recipe).unwrap();
    let revisions: HashMap<String, String> =
        scope.regions.iter().map(|region| (region.id.clone(), region.revision_hash.clone())).collect();
    let reread = |_: &Path, _: u32, id: &str, _: &RenderRecipe| Ok(RenderInput {
        crop_bounds: template.crop_bounds, crop_png: Vec::new(), crop_sha256: template.crop_sha256.clone(),
        hint_png: Vec::new(), hint_sha256: template.hint_sha256.clone(), source_hash: template.source_hash.clone(),
        mask_hash: template.mask_hash.clone(), revision: template.revision,
        revision_hash: revisions.get(id).cloned().expect("only planned regions are read"),
        input_sha256: template.input_sha256.clone(), predecessors_sha256: template.predecessors_sha256.clone(),
        source_idx: template.source_idx,
    });
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false),
        &Harness { read: &reread, ..OPEN });

    let rendered = renderer.rendered();
    assert_eq!(rendered.len(), GROUPS);
    assert_eq!(rendered.iter().collect::<HashSet<_>>().len(), GROUPS, "a region was sent twice");
    assert!(rendered.iter().all(|id| ids.contains(id)), "a region outside the plan was sent");
    // Checked before each chunk, and a chunk starts only once the last is done.
    assert_eq!(*renderer.rendered_at_deployment.lock().unwrap(), vec![0, 256, 512, 768, 1024]);
    for (chunk, range) in [0..256, 256..512, 512..768, 768..1024, 1024..1025].into_iter().enumerate() {
        let sent: HashSet<&String> = rendered[range.clone()].iter().collect();
        assert_eq!(sent, ids[range].iter().collect(), "chunk {chunk} is its own slice of the plan");
    }
    assert_eq!((summary.reason, summary.regions_cleaned as usize, summary.pages_queued as usize),
        ("completed", GROUPS, pages));
    assert_eq!(of_type(&events, "region-done"), GROUPS);
    assert!(notices(&events, "notice.cloudClean.stopped").is_empty());
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 1);
    assert!(Job::open(&path).unwrap().project.patches.is_empty(), "nothing was cleaned here");
}

/// Cancelling between chunks: the chunk that ran is kept, and no later chunk
/// sends anything.
#[test]
fn cancel_between_chunks_stops_later_chunks() {
    let (path, ids) = three_chunks("chunk-cancel");
    let grants = Arc::new(GrantService::new());
    let scope = plan_for(&path, &ids, &grants, 2, CleanExecution::Cloud);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    let flag = Arc::clone(&cancel);
    renderer.on_deployment = Box::new(move |reads| if reads == 2 { flag.store(true, Ordering::SeqCst) });
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &cancel, &OPEN);

    let mut rendered = renderer.rendered();
    rendered.sort();
    assert_eq!(rendered, ids[..2].to_vec());
    assert_eq!(detection_ids(&path), ids[2..].to_vec());
    assert_eq!((summary.reason, summary.regions_cleaned, summary.next_page_index), ("cancelled", 2, Some(1)));
    assert!(notices(&events, "notice.cloudClean.stopped").is_empty());
    assert_eq!(of_type(&events, "page-started"), of_type(&events, "page-done"));
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 1);
}

/// Calls `then` as the `nth` render starts (1-based).
fn at_render(nth: usize, then: impl Fn() + Sync + Send + 'static) -> Box<dyn Fn(&str) + Sync> {
    let seen = AtomicUsize::new(0);
    Box::new(move |_| if seen.fetch_add(1, Ordering::SeqCst) + 1 == nth { then() })
}

/// A profile edited (its epoch moves: a new key, a changed address) between
/// chunks stops every later chunk before it is minted, with one notice.
#[test]
fn a_profile_change_between_chunks_stops_later_chunks() {
    let (path, ids) = three_chunks("chunk-epoch");
    let grants = Arc::new(GrantService::new());
    let scope = plan_for(&path, &ids, &grants, 2, CleanExecution::Cloud);
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    let edits = Arc::clone(&grants);
    // As the first chunk's last region renders: that region still lands.
    renderer.on_render = at_render(2, move || {
        edits.invalidate_profile(CloudProvider::Modal, "modal-prof-clean-run");
    });
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false), &OPEN);

    let mut rendered = renderer.rendered();
    rendered.sort();
    assert_eq!(rendered, ids[..2].to_vec());
    assert_eq!(detection_ids(&path), ids[2..].to_vec(), "a later chunk was sent");
    let stopped = notices(&events, "notice.cloudClean.stopped");
    assert_eq!((stopped.len(), stopped[0]["code"].as_str(), stopped[0]["page"].as_u64()),
        (1, Some("cloud_run_profile_changed"), Some(2)));
    assert_eq!(renderer.rendered_at_deployment.lock().unwrap().len(), 1, "the next chunk was not even checked");
    assert_eq!((summary.reason, summary.regions_cleaned), ("completed", 2));
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 0, "a run that lost its endpoint asks it nothing");
    assert_eq!(of_type(&events, "page-started"), of_type(&events, "page-done"));
}

/// Another profile selected, or cloud engines turned off, between chunks:
/// the run no longer speaks for its endpoint, and stops the same way.
#[test]
fn a_profile_deselected_between_chunks_stops_later_chunks() {
    let (path, ids) = three_chunks("chunk-deselected");
    let grants = Arc::new(GrantService::new());
    let scope = plan_for(&path, &ids, &grants, 2, CleanExecution::Cloud);
    let selected = Arc::new(AtomicBool::new(true));
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    let switch = Arc::clone(&selected);
    renderer.on_render = at_render(2, move || switch.store(false, Ordering::SeqCst));
    let allowed = {
        let selected = Arc::clone(&selected);
        move || if selected.load(Ordering::SeqCst) { Ok(()) } else { Err("cloud_run_profile_changed") }
    };
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false),
        &Harness { allowed: &allowed, ..OPEN });

    let mut rendered = renderer.rendered();
    rendered.sort();
    assert_eq!(rendered, ids[..2].to_vec());
    assert_eq!(detection_ids(&path), ids[2..].to_vec());
    let stopped = notices(&events, "notice.cloudClean.stopped");
    assert_eq!((stopped.len(), stopped[0]["code"].as_str()), (1, Some("cloud_run_profile_changed")));
    assert_eq!((summary.reason, summary.regions_cleaned), ("completed", 2));
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 0);
}

/// A redeployed model, or another GPU than the one the estimate priced,
/// stops the later chunks: the consent was for that deployment.
#[test]
fn a_changed_gpu_or_recipe_between_chunks_stops_later_chunks() {
    let a100 = Some(("A100".to_string(), 2.5));
    let redeployed = RenderRecipe { model_revision: "e".repeat(40), ..recipe() };
    for (code, second) in [
        ("cloud_clean_gpu_changed", (recipe(), a100)),
        ("cloud_clean_gpu_changed", (recipe(), Some(("L4".to_string(), 0.9)))),
        ("cloud_clean_gpu_changed", (recipe(), None)),
        ("cloud_clean_recipe_changed", (redeployed, l4())),
    ] {
        let (path, ids) = three_chunks("chunk-deployment");
        let grants = Arc::new(GrantService::new());
        let scope = plan_for(&path, &ids, &grants, 2, CleanExecution::Cloud);
        let mut renderer = Renderer::new(&path, &grants);
        renderer.delay = Duration::ZERO;
        renderer.deployed = vec![(recipe(), l4()), second];
        let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false), &OPEN);

        let stopped = notices(&events, "notice.cloudClean.stopped");
        assert_eq!((stopped.len(), stopped[0]["code"].as_str()), (1, Some(code)));
        assert_eq!(detection_ids(&path), ids[2..].to_vec(), "{code}: a later chunk was sent");
        assert_eq!(summary.regions_cleaned, 2);
        // The run still speaks for the endpoint: the GPU the first chunk woke
        // is released, not billed through its idle window.
        assert_eq!(renderer.releases.load(Ordering::SeqCst), 1, "{code}: the woken GPU was not released");
    }
}

/// An unpriced plan may continue while the price stays unavailable, but a
/// newly stated price changes the terms shown at consent and stops the next
/// chunk before it renders.
#[test]
fn an_unpriced_plan_stops_when_a_price_becomes_available() {
    let (path, ids) = three_chunks("chunk-unpriced");
    let grants = Arc::new(GrantService::new());
    let mut scope = plan_for(&path, &ids, &grants, 2, CleanExecution::Cloud);
    scope.price = None;
    scope.cost = None;
    scope.plan_digest = scope.digest().unwrap();
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    // Unreadable for two chunks, then stated before the third starts.
    renderer.deployed = vec![(recipe(), None), (recipe(), None), (recipe(), l4())];
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false), &OPEN);

    let stopped = notices(&events, "notice.cloudClean.stopped");
    assert_eq!((stopped.len(), stopped[0]["code"].as_str()), (1, Some("cloud_clean_gpu_changed")));
    assert_eq!((summary.reason, summary.regions_cleaned), ("completed", 4));
    assert_eq!(detection_ids(&path), ids[4..].to_vec());
    assert_eq!(renderer.rendered_at_deployment.lock().unwrap().len(), 3);
}

/// A chunk is provably a subset of the plan: no id from outside it, none from
/// another chunk, none out of order or twice, none under another plan. A
/// chunk that fails the proof sends nothing.
#[test]
fn a_chunk_never_includes_an_id_outside_the_approved_scope() {
    let ordered = ["a", "b", "c", "d", "e"];
    let plan = "p".repeat(64);
    let chunk = |index: usize, ids: &[&str]| ChunkGrant {
        plan_digest: plan.clone(), index, ids: ids.iter().map(|id| id.to_string()).collect(),
    };
    let verify = |grant: &ChunkGrant| verify_chunk_grant(&plan, &ordered, 2, MAX_CLEAN_REGIONS, grant);
    assert_eq!(verify(&chunk(0, &["a", "b"])), Ok(vec![0, 1]));
    assert_eq!(verify(&chunk(1, &["d"])), Ok(vec![3]), "a chunk may shrink");
    assert_eq!(verify(&chunk(2, &["e"])), Ok(vec![4]));
    assert_eq!(verify(&chunk(0, &["a", "z"])), Err("cloud_clean_chunk_outside_plan"));
    assert_eq!(verify(&chunk(0, &["a", "c"])), Err("cloud_clean_chunk_outside_plan"), "c is chunk 1's");
    assert_eq!(verify(&chunk(1, &["d", "c"])), Err("cloud_clean_chunk_outside_plan"), "out of order");
    assert_eq!(verify(&chunk(1, &["c", "c"])), Err("cloud_clean_chunk_outside_plan"), "twice");
    assert_eq!(verify(&chunk(3, &[])), Err("cloud_clean_chunk_mismatch: index"));
    let other = ChunkGrant { plan_digest: "q".repeat(64), ..chunk(0, &["a"]) };
    assert_eq!(verify(&other), Err("cloud_clean_chunk_mismatch: plan"));
    assert_eq!(verify_chunk_grant(&plan, &ordered, MAX_CLEAN_REGIONS + 1, MAX_CLEAN_REGIONS, &chunk(0, &["a"])),
        Err("cloud_clean_chunk_mismatch: size"));
    // Minting only ever takes the chunk's own slice, and what `keep` allows.
    for index in 0..3 {
        let minted = issue_chunk_grant(&plan, &ordered, 2, index, &mut |position| position != 3).unwrap();
        assert!(verify(&minted).is_ok());
        assert!(minted.ids.iter().all(|id| ordered[index * 2..(index * 2 + 2).min(5)].contains(&id.as_str())));
    }
    assert!(issue_chunk_grant(&plan, &ordered, 2, 3, &mut |_| true).is_none());

    // The run refuses a chunk naming a region outside the plan before any
    // region of it starts, and stops.
    let (path, ids) = three_chunks("chunk-outside");
    let outsider = detect(&path, 0, Rect::new(100, 100, 20, 12), REMOTE_PICK, 2);
    let grants = Arc::new(GrantService::new());
    let scope = plan_for(&path, &ids, &grants, 2, CleanExecution::Cloud);
    let renderer = Renderer::new(&path, &grants);
    let events = RefCell::new(Vec::new());
    let emit = |event: Event| events.borrow_mut().push(event);
    let batch = Batch {
        scope: &scope, job_path: &path, grants: &grants, allowed: OPEN.allowed, unresolved: &settled,
        local_pass: None, read: &read_render_input,
    };
    let mut progress = Progress::new(&scope, &path, "run-test", &emit);
    let mut slots = vec![Slot::Waiting; scope.regions.len()];
    let widened = ChunkGrant { plan_digest: scope.plan_digest.clone(), index: 0, ids: vec![ids[0].clone(), outsider] };
    assert_eq!(run_chunk(&batch, &renderer, &widened, &mut slots, &mut progress, &AtomicBool::new(false)),
        Err("cloud_clean_chunk_outside_plan"));
    assert!(renderer.rendered().is_empty());
    assert!(slots.iter().all(|slot| *slot == Slot::Waiting), "nothing was queued");
}

/// Strictly the cloud: fill and solid picks are rendered on the cloud GPU like
/// any other, and the local pass is never run, even when one is at hand.
#[test]
fn strict_cloud_only_never_runs_a_local_fill() {
    let path = chapter("strict", 2, StripMode::Single);
    let ids = vec![
        detect(&path, 0, Rect::new(100, 100, 24, 16), "fill", 0),
        detect(&path, 1, Rect::new(20, 20, 24, 16), "solid", 0),
    ];
    let proposal = prepare(&path, asked("chapter", false), destination("modal-prof-clean-strict"),
        &Gateway::priced(), &settled).unwrap();
    assert_eq!((proposal.execution, proposal.local_candidates, proposal.region_ids.clone()),
        (CleanExecution::Cloud, 0, ids.clone()));

    let grants = Arc::new(GrantService::new());
    let scope = scope_for(&path, &ids, &grants);
    assert!(scope.regions.iter().all(|region| region.local_candidate));
    let fills = AtomicUsize::new(0);
    let local_pass = |ids: &[String]| {
        fills.fetch_add(1, Ordering::SeqCst);
        run::clean_fill_rung_first(&path, ids, &AtomicBool::new(false))
    };
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    let (summary, _) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false),
        &Harness { local_pass: Some(&local_pass), ..OPEN });

    assert_eq!(fills.load(Ordering::SeqCst), 0, "a local fill ran in strict cloud-only mode");
    let mut rendered = renderer.rendered();
    rendered.sort();
    assert_eq!(rendered, ids);
    assert_eq!(summary.regions_cleaned, 2);
    let job = Job::open(&path).unwrap();
    assert!(job.project.patches.iter().all(|row| job.load_patch(row).unwrap().provenance.engine == Engine::Flux));
}

/// Mixed, chosen by name: after the run starts, the fill is cleaned here and
/// leaves the plan; the rest go to the cloud. Nothing happened at prepare.
#[test]
fn mixed_execution_cleans_fills_here_only_after_the_run_starts() {
    let path = chapter("mixed-run", 2, StripMode::Single);
    let fill = detect(&path, 0, Rect::new(100, 100, 24, 16), "fill", 0);
    let remote = detect(&path, 1, Rect::new(100, 100, 30, 20), REMOTE_PICK, 0);
    let ids = vec![fill.clone(), remote.clone()];
    let grants = Arc::new(GrantService::new());
    let scope = plan_for(&path, &ids, &grants, CHUNK_REGIONS, CleanExecution::Mixed);
    assert!(Job::open(&path).unwrap().project.patches.is_empty());
    let passes = AtomicUsize::new(0);
    let local_pass = |ids: &[String]| {
        passes.fetch_add(1, Ordering::SeqCst);
        assert_eq!(ids, std::slice::from_ref(&fill), "only the fill is tried here");
        run::clean_fill_rung_first(&path, ids, &AtomicBool::new(false))
    };
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false),
        &Harness { local_pass: Some(&local_pass), ..OPEN });

    assert_eq!(passes.load(Ordering::SeqCst), 1);
    assert_eq!(renderer.rendered(), vec![remote.clone()], "the fill cleaned here was also sent");
    let job = Job::open(&path).unwrap();
    let engine = |id: &str| job.load_patch(job.project.patches.iter().find(|row| row.id == id).unwrap()).unwrap()
        .provenance.engine;
    assert_eq!((engine(&fill), engine(&remote)), (Engine::Fill, Engine::Flux));
    assert_eq!((summary.regions_cleaned, of_type(&events, "region-done")), (2, 2));
    assert_eq!(of_type(&events, "page-started"), of_type(&events, "page-done"));
}

/// Two fills and a remote on each of two pages, the chunk the size of a page.
fn mixed_pages(name: &str) -> (PathBuf, [[String; 3]; 2]) {
    let path = chapter(name, 2, StripMode::Single);
    let page = |index: usize| [
        detect(&path, index, Rect::new(20, 20, 24, 16), "fill", 0),
        detect(&path, index, Rect::new(100, 100, 24, 16), "fill", 1),
        detect(&path, index, Rect::new(40, 180, 30, 20), REMOTE_PICK, 2),
    ];
    let pages = [page(0), page(1)];
    (path, pages)
}

/// Mixed runs its local pass chunk by chunk, a page per call, just before
/// that chunk goes to the cloud: the job is never held for the whole plan.
#[test]
fn mixed_cleans_here_a_page_at_a_time_just_before_each_chunk() {
    let (path, pages) = mixed_pages("mixed-chunks");
    let ids: Vec<String> = pages.iter().flatten().cloned().collect();
    let grants = Arc::new(GrantService::new());
    let scope = plan_for(&path, &ids, &grants, 3, CleanExecution::Mixed);
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let local_pass = {
        let log = Arc::clone(&log);
        let path = path.clone();
        move |ids: &[String]| {
            log.lock().unwrap().push(format!("here {}", ids.join(",")));
            run::clean_fill_rung_first(&path, ids, &AtomicBool::new(false))
        }
    };
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    let cloud = Arc::clone(&log);
    renderer.on_render = Box::new(move |id| cloud.lock().unwrap().push(format!("cloud {id}")));
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false),
        &Harness { local_pass: Some(&local_pass), ..OPEN });

    let [first, second] = &pages;
    assert_eq!(*log.lock().unwrap(), vec![
        format!("here {},{}", first[0], first[1]),
        format!("cloud {}", first[2]),
        format!("here {},{}", second[0], second[1]),
        format!("cloud {}", second[2]),
    ]);
    assert_eq!((summary.reason, summary.regions_cleaned, of_type(&events, "region-done")), ("completed", 6, 6));
    assert!(detection_ids(&path).is_empty());
}

/// A cancel is heard between pages of the local pass: the next page is not
/// cleaned here and nothing is sent.
#[test]
fn a_cancel_stops_mixeds_local_pass_before_the_next_page() {
    let (path, pages) = mixed_pages("mixed-cancel");
    let ids: Vec<String> = pages.iter().flatten().cloned().collect();
    let grants = Arc::new(GrantService::new());
    let scope = plan_for(&path, &ids, &grants, CHUNK_REGIONS, CleanExecution::Mixed);
    let cancel = AtomicBool::new(false);
    let passes = AtomicUsize::new(0);
    let local_pass = |ids: &[String]| {
        passes.fetch_add(1, Ordering::SeqCst);
        let cleaned = run::clean_fill_rung_first(&path, ids, &AtomicBool::new(false));
        cancel.store(true, Ordering::SeqCst);
        cleaned
    };
    let renderer = Renderer::new(&path, &grants);
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &cancel,
        &Harness { local_pass: Some(&local_pass), ..OPEN });

    assert_eq!(passes.load(Ordering::SeqCst), 1, "a page was cleaned here after the cancel");
    assert!(renderer.rendered().is_empty(), "a cancelled run sent a region");
    assert_eq!((summary.reason, summary.regions_cleaned, of_type(&events, "region-done")), ("cancelled", 2, 2));
    let mut left = pages[0][2..].to_vec();
    left.extend(pages[1].iter().cloned());
    assert_eq!(detection_ids(&path), left);
    assert_eq!(of_type(&events, "page-started"), of_type(&events, "page-done"));
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 0, "nothing was sent, so nothing woke");
}

/// A local pass that fails partway has already saved the regions it cleaned
/// before the failure. Those are settled as cleaned, with their `region-done`,
/// and not later skipped as changed; the rest go to the cloud as planned.
#[test]
fn a_local_pass_that_fails_partway_settles_what_it_saved() {
    let (path, pages) = mixed_pages("mixed-partway");
    let ids: Vec<String> = pages[0].to_vec();
    let grants = Arc::new(GrantService::new());
    let scope = plan_for(&path, &ids, &grants, CHUNK_REGIONS, CleanExecution::Mixed);
    let local_pass = |ids: &[String]| {
        assert_eq!(run::clean_fill_rung_first(&path, &ids[..1], &AtomicBool::new(false)).unwrap(), ids[..1].to_vec());
        Err::<Vec<String>, String>("job_busy: another Manga Cleaner process is using it".into())
    };
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false),
        &Harness { local_pass: Some(&local_pass), ..OPEN });

    let [saved, unsaved, remote] = &pages[0];
    assert_eq!(renderer.rendered(), vec![unsaved.clone(), remote.clone()], "the saved fill was sent again");
    assert!(notices(&events, "notice.cloudClean.regionSkipped").is_empty(), "a saved fill was called changed");
    let done: Vec<String> = events.iter().filter_map(|event| match event {
        Event::RegionDone { region, .. } => Some(region.id.clone()),
        _ => None,
    }).collect();
    assert_eq!(done.first(), Some(saved));
    assert_eq!((summary.reason, summary.regions_cleaned), ("completed", 3));
    let job = Job::open(&path).unwrap();
    let engine = |id: &str| job.load_patch(job.project.patches.iter().find(|row| row.id == *id).unwrap()).unwrap()
        .provenance.engine;
    assert_eq!((engine(saved), engine(unsaved), engine(remote)), (Engine::Fill, Engine::Flux, Engine::Flux));
}

/// Reading back what the local pass committed is a read: it does not wait for
/// the job lock, so a writer holding the chapter does not cost the run its
/// `region-done` events.
#[test]
fn reading_back_local_cleans_does_not_wait_for_the_job() {
    let path = chapter("read-back", 1, StripMode::Single);
    let fill = detect(&path, 0, Rect::new(100, 100, 24, 16), "fill", 0);
    let remote = detect(&path, 0, Rect::new(40, 180, 30, 20), REMOTE_PICK, 1);
    assert_eq!(run::clean_fill_rung_first(&path, std::slice::from_ref(&fill), &AtomicBool::new(false)).unwrap(), vec![fill.clone()]);
    let (held, release) = (mpsc::channel(), mpsc::channel::<()>());
    let holder = {
        let path = path.clone();
        let (held, release) = (held.0, release.1);
        std::thread::spawn(move || {
            let _lock = run::lock_job(&path).unwrap();
            held.send(()).unwrap();
            let _ = release.recv_timeout(Duration::from_secs(10));
        })
    };
    held.1.recv().unwrap();
    let asked = std::time::Instant::now();
    let found = committed_regions(&path, "chapter", 0, &[fill.clone(), remote.clone()]);
    let waited = asked.elapsed();
    release.0.send(()).unwrap();
    holder.join().unwrap();

    assert!(waited < Duration::from_secs(2), "the read waited {waited:?} for the job");
    let found = found.unwrap();
    assert_eq!(found.keys().cloned().collect::<Vec<_>>(), vec![fill.clone()], "only the patch is committed");
    assert_eq!(found[&fill].id, fill);
}

/* ------------------------------------------------------------------ */
/* LaMa picks go to the cloud too                                      */
/* ------------------------------------------------------------------ */

/// Clean on Cloud GPU means the cloud: a LaMa pick is planned, counted and
/// rendered there like any other, and nothing is cleaned by LaMa here.
#[test]
fn lama_picks_are_planned_and_rendered_on_the_cloud() {
    let path = chapter("lama-cloud", 2, StripMode::Single);
    let lama0 = detect(&path, 0, Rect::new(40, 180, 30, 20), "lama", 0);
    let fill = detect(&path, 0, Rect::new(150, 40, 24, 16), "fill", 1);
    let lama1 = detect(&path, 1, Rect::new(40, 180, 30, 20), "lama", 0);
    let ids = vec![lama0.clone(), fill.clone(), lama1.clone()];
    let proposal = prepare(&path, asked("chapter", false), destination("modal-prof-clean-lama"), &Gateway::priced(),
        &settled).unwrap();
    if let Some(id) = proposal.proposal_id.as_deref() { discard(id).unwrap(); }
    assert_eq!((proposal.region_ids.clone(), proposal.regions, proposal.pages), (ids.clone(), 3, 2));

    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    let (summary, _) = run_with(&path, &ids, &grants, &renderer, &AtomicBool::new(false));
    let mut rendered = renderer.rendered();
    rendered.sort();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(rendered, expected, "a LaMa pick was not sent");
    assert_eq!((summary.reason, summary.regions_cleaned), ("completed", 3));
    let job = Job::open(&path).unwrap();
    assert!(job.project.patches.iter().all(|row| job.load_patch(row).unwrap().provenance.engine == Engine::Flux));
}

/// Consent comes before any upload: preparing and confirming read only the
/// gateway's model and price, and not one region reaches the renderer until
/// the run spends the grant.
#[test]
fn nothing_is_uploaded_before_consent() {
    let profile = "modal-prof-clean-upload";
    let path = chapter("no-upload", 2, StripMode::Single);
    let ids: Vec<String> = (0..2).map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
    let grants = Arc::new(GrantService::new());
    let renderer = Renderer::new(&path, &grants);
    let gateway = Gateway::priced();

    let declined = prepare(&path, asked("chapter", false), destination(profile), &gateway, &settled).unwrap();
    assert!(discard(declined.proposal_id.as_deref().unwrap()).unwrap());
    let proposal = prepare(&path, asked("chapter", false), destination(profile), &gateway, &settled).unwrap();
    assert_eq!(agree(proposal.proposal_id.as_deref().unwrap(), true, false).unwrap_err(),
        "retention_acknowledgement_required");
    let grant = agree(proposal.proposal_id.as_deref().unwrap(), true, true).unwrap();

    assert!(renderer.rendered().is_empty(), "a region was sent before the run");
    assert_eq!((gateway.asked.load(Ordering::SeqCst), gateway.priced.load(Ordering::SeqCst)), (2, 2),
        "only the model and the price were read");
    assert!(grants.is_empty(), "no region grant exists before the run");
    assert_eq!(detection_ids(&path), ids);
    assert!(consume_grant(&grant.grant_id, &request(profile, &recipe())).is_ok());
}

/// An earlier request whose outcome is unresolved keeps its region out of the
/// plan, and a region that becomes unresolved after consent is left out of
/// its chunk, with a notice, rather than sent a second time.
#[test]
fn unresolved_attempts_are_left_out_of_the_plan_and_of_new_chunks() {
    let (path, ids) = three_chunks("unresolved");
    let held = ids[1].clone();
    let proposal = prepare(&path, asked("chapter", false), destination("modal-prof-clean-unresolved"),
        &Gateway::priced(), &|_: u32, id: &str| id == held).unwrap();
    assert_eq!(proposal.unresolved_ids, vec![held.clone()]);
    assert!(!proposal.region_ids.contains(&held));
    assert_eq!(proposal.regions, 5);

    // Everything held: nothing to consent to, and the gateway is not asked.
    let gateway = Gateway::priced();
    let all_held = prepare(&path, asked("chapter", false), destination("modal-prof-clean-unresolved"), &gateway,
        &|_: u32, _: &str| true).unwrap();
    assert_eq!((all_held.regions, all_held.proposal_id.clone(), all_held.unresolved_ids.clone()), (0, None, ids.clone()));
    assert_eq!(gateway.asked.load(Ordering::SeqCst), 0);

    let grants = Arc::new(GrantService::new());
    let scope = plan_for(&path, &ids, &grants, 2, CleanExecution::Cloud);
    let late = ids[3].clone();
    let unresolved = move |_: u32, id: &str| id == late;
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::ZERO;
    let (summary, events) = run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false),
        &Harness { unresolved: &unresolved, ..OPEN });
    assert!(!renderer.rendered().contains(&ids[3]), "an unresolved region was sent again");
    let skipped = notices(&events, "notice.cloudClean.regionSkipped");
    assert_eq!((skipped.len(), skipped[0]["regionId"].as_str(), skipped[0]["reason"].as_str()),
        (1, Some(ids[3].as_str()), Some("unresolved")));
    assert_eq!(detection_ids(&path), vec![ids[3].clone()]);
    assert_eq!(summary.regions_cleaned, 5);
    assert_eq!(of_type(&events, "page-started"), of_type(&events, "page-done"));
}

/// The app asks the attempt journal, under the chapter and page the render
/// path records: a submission left unknown blocks its region and no other.
#[test]
fn the_journal_answers_which_regions_are_unresolved() {
    let (path, ids) = three_chunks("journal");
    let root = path.parent().unwrap().join("journal");
    let journal = AttemptJournal::new(&root, cleaner_core::cloud_wire::provisional_fixture_limits());
    let request: cleaner_core::cloud_wire::JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
    let intent = CreateAttemptIntent {
        qwen_edit: None,
        attempt_id: request.attempt_id.clone(), job_id: request.job_id.clone(), provider: CloudProvider::Modal,
        profile_id: "modal-prof-clean-journal".into(), endpoint_fingerprint: FINGERPRINT.clone(),
        recipe_id: request.recipe.recipe_id.clone(), preprocessing_version: request.recipe.preprocessing_version.clone(),
        model_id: request.recipe.model_id.clone(), model_revision: request.recipe.model_revision.clone(),
        native_mask_conditioning: request.recipe.native_mask_conditioning, width: request.width,
        height: request.height, seed: request.seed, steps: request.steps, guidance_scaled: request.guidance_scaled,
        source_image_hash: "2".repeat(64), region_id: ids[2].clone(), region_revision: 1, input_sha256: None,
        predecessors_sha256: None, crop_sha256: request.image_sha256.clone(), hint_sha256: request.hint_sha256.clone(),
        request_digest: cleaner_core::cloud_wire::compute_request_digest(&request),
        chapter_id: Some("chapter".into()), page_index: Some(1),
    };
    let (_, guard) = journal.create_intent(intent).unwrap();
    let _permit = journal.mark_dispatching(&guard).unwrap();
    journal.record_dispatch_transport_error(&guard).unwrap();

    let unresolved = journal_unresolved(journal.clone(), &path);
    assert!(unresolved(1, &ids[2]));
    assert!(!unresolved(1, &ids[3]) && !unresolved(0, &ids[2]));
    let proposal = prepare(&path, asked("chapter", false), destination("modal-prof-clean-journal"),
        &Gateway::priced(), &unresolved).unwrap();
    assert_eq!(proposal.unresolved_ids, vec![ids[2].clone()]);
}
