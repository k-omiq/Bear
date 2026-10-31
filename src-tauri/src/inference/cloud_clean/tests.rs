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
        reading: None,
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
        work_long_side: None,
        flux3_generation: None,
        finishing: Default::default(),
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

/// A rejected result leaves an ordinary detection: the next cloud clean
/// plans it, bound to the revision the first plan bound.
#[test]
fn a_rejected_result_is_planned_again_as_the_detection_it_was() {
    let path = chapter("reject-replan", 1, StripMode::Single);
    let id = detect(&path, 0, Rect::new(100, 100, 30, 20), REMOTE_PICK, 0);
    let gateway = Gateway::priced();
    // Each plan is let go once read: the proposals are one cache for every test.
    let plan = |profile: &str| {
        let proposal = prepare(&path, asked("chapter", false), destination(profile), &gateway, &settled).unwrap();
        if let Some(id) = proposal.proposal_id.as_deref() { discard(id).unwrap(); }
        proposal
    };
    assert_eq!(plan("modal-prof-clean-replan-a").region_ids, vec![id.clone()]);
    let bound = |path: &Path| bind(path, std::slice::from_ref(&id), recipe_preprocessing(&recipe()).unwrap()).unwrap().regions;
    let first = bound(&path);

    // The result is applied at once, as a cloud render commits it.
    {
        let _lock = run::lock_job(&path).unwrap();
        let mut job = Job::open(&path).unwrap();
        let loaded = job.load_detection(&id).unwrap().unwrap();
        let bounds = loaded.mask.bounds;
        let pixels = Raster { width: bounds.w, height: bounds.h, data: vec![200; (bounds.w * bounds.h) as usize],
            ..page_raster() };
        let patch = Patch {
            id: id.clone(), mask: loaded.mask, ink: loaded.ink, pixels, order: loaded.record.order, visible: true,
            provenance: Provenance {
                engine: Engine::Flux, engine_version: "test".into(), model_sha256: None,
                execution_provider: "cloud".into(), params_snapshot: serde_json::json!({}),
                mask_sha256: String::new(), source_sha256: String::new(), cloud: None, created: 1_760_000_000,
            },
        };
        assert!(job.complete_detection(&id, &patch, None).unwrap());
        library::invalidate_manifest_cache(&path);
    }
    assert!(plan("modal-prof-clean-replan-b").region_ids.is_empty(), "a cleaned region is not planned");

    // Rejected.
    {
        let _lock = run::lock_job(&path).unwrap();
        Job::open(&path).unwrap().restore_detection(&id).unwrap();
        library::invalidate_manifest_cache(&path);
    }
    let again = plan("modal-prof-clean-replan-c");
    assert_eq!(again.region_ids, vec![id.clone()]);
    assert!(again.unresolved_ids.is_empty());
    assert_eq!(format!("{:?}", bound(&path)), format!("{first:?}"), "the same region, at the same revision");
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
        cache.lock().unwrap().insert(n.to_string(), StoredProposal { expires_at_ms, scope: scope.clone(), stream: None });
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
    assert_eq!(working_size(96, 48, WORK_LONG_SIDE), (768, 384));
    assert_eq!(working_size(100, 60, WORK_LONG_SIDE), (768, 448));
    assert_eq!(working_size(1024, 512, WORK_LONG_SIDE), (1024, 512));
    assert_eq!(working_size(16, 16, WORK_LONG_SIDE), (768, 768));

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
    let (w, h) = working_size(crop.w, crop.h, WORK_LONG_SIDE);
    let per_job = JOB_OVERHEAD_SECONDS + f64::from(w * h) / 1e6 * SECONDS_PER_WORK_MEGAPIXEL;
    assert!((small.gpu_seconds - 2.0 * per_job).abs() < 1e-9);
    assert_eq!(small.seconds().high, small.gpu_seconds + COLD_START_SECONDS + IDLE_WINDOW_SECONDS);

    assert_eq!(IDLE_WINDOW_SECONDS, 600.0, "high covers the deployment's maximum idle setting");
    // FLUX 4B holds two pipeline copies (deploy/cloud/common/capacity.py):
    // 12 + 7 GiB in one Modal process, two 12 GiB Beam workers.
    let per_second = |memory: f64| 3.6 / 3600.0 + CPU_CORES * CPU_USD_PER_CORE_SECOND + memory * MEMORY_USD_PER_GIB_SECOND;
    let priced = cost(80.0, 0.0, 3.6, &recipe(), CloudProvider::Modal);
    assert!((priced.low - 80.0 * per_second(19.0)).abs() < 1e-12);
    assert!((priced.high - (80.0 + COLD_START_SECONDS + IDLE_WINDOW_SECONDS) * per_second(19.0)).abs() < 1e-12);
    let beam = cost(80.0, 0.0, 3.6, &recipe(), CloudProvider::Beam);
    assert!((beam.low - 80.0 * per_second(24.0)).abs() < 1e-12);
    let nine = RenderRecipe { model_id: "Disty0/FLUX.2-klein-9B-SDNQ".into(), ..recipe() };
    assert_eq!(RenderRate::of(&nine).memory_gib(CloudProvider::Modal), 48.0, "the 9B holds two copies on the Modal L40S");
    assert_eq!(RenderRate::of(&nine).memory_gib(CloudProvider::Beam), 24.0, "and one on the Beam RTX5090");
    assert!(cost(80.0, 0.0, 3.6, &nine, CloudProvider::Modal).low > priced.low, "the 9B worker has more memory");
    let flux3 = RenderRecipe { model_id: "OzzyGT/Flux_3_Image_Klein_sdnq_dynamic_4bit".into(), ..recipe() };
    let rate = RenderRate::of(&flux3);
    assert_eq!((rate.memory_gib(CloudProvider::Modal), rate.memory_gib(CloudProvider::Beam)), (24.0, 24.0),
        "FLUX 3 holds one copy in 24 GiB on both");
    assert_eq!(rate.working_size(96, 48), working_size(96, 48, WORK_LONG_SIDE), "FLUX 3 works at Klein's size");
}

/// The `cloudWorkLongSide` setting reaches a FLUX 3 recipe, which the worker
/// and the estimate both read; the worker's own size travels as none, and a
/// recipe that takes no size leaves the setting unread.
#[test]
fn the_working_size_setting_reaches_a_flux3_recipe_and_its_estimate() {
    let flux3 = RenderRecipe::new("mc-flux3-klein-bf16-edit-v2", "3.0.0", "unsloth/flux-3-image-klein",
        "67662613c10c92a1364cb3da8c6f7447977c7ce9", false);
    let gateway = Gateway { recipe: flux3.clone(), ..Gateway::priced() };
    let sized = deployed_recipe(&gateway, None, Some(1280)).unwrap();
    assert_eq!(sized.work_long_side, Some(1280));
    assert!(sized.same_deployment(&flux3), "a working size is request-local");
    assert_eq!(deployed_recipe(&gateway, None, Some(WORK_LONG_SIDE)).unwrap().work_long_side, None);
    assert_eq!(deployed_recipe(&gateway, None, None).unwrap().work_long_side, None);
    assert_eq!(deployed_recipe(&Gateway::priced(), None, Some(1280)).unwrap().work_long_side, None,
        "Klein takes no working size");

    let rate = RenderRate::of(&sized);
    assert_eq!(rate.working_size(96, 48), (1280, 640));
    assert_eq!(rate.working_size(1400, 700), (1400, 700), "never scaled down");
    assert_eq!((rate.memory_gib(CloudProvider::Modal), rate.memory_gib(CloudProvider::Beam)), (40.0, 40.0),
        "the bf16 worker is 40 GiB");
    let own_size = RenderRate::of(&deployed_recipe(&gateway, None, None).unwrap());
    let crop = Rect::new(0, 0, 96, 48);
    assert!(Estimate::job(crop, rate).gpu_seconds > 2.0 * Estimate::job(crop, own_size).gpu_seconds);

    for (value, side) in [(serde_json::Value::Null, None), (serde_json::json!(1024), Some(1024))] {
        assert_eq!(work_long_side_from_value(Some(&value)), Ok(side));
    }
    assert_eq!(work_long_side_from_value(None), Ok(None));
    assert!(work_long_side_from_value(Some(&serde_json::json!(1100))).is_err());
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
    assert_eq!(RenderRate::of(&recipe()).working_size(96, 48), working_size(96, 48, WORK_LONG_SIDE));
    let region = GrantedRegion {
        id: "r".into(), page_index: 0, order: 0, bbox: Rect::new(0, 0, 20, 10), space: 0, revision_hash: String::new(),
        crop: cleaner_core::engines::render::crop_for(Rect::new(0, 0, 20, 10)).unwrap(), group_id: None,
        local_candidate: false,
    };
    let estimate = Estimate::of(std::slice::from_ref(&region), rate);
    assert!((estimate.seconds().high - estimate.gpu_seconds - estimate.retry_gpu_seconds - 90.0 - IDLE_WINDOW_SECONDS).abs() < 1e-9);
    assert!(estimate.retry_gpu_seconds > 0.0);
    // The dollars cover what the seconds do, the retries too.
    let per_second = cost(1.0, 0.0, 3.6, &qwen, CloudProvider::Modal).low;
    let priced = cost(estimate.gpu_seconds, estimate.retry_gpu_seconds, 3.6, &qwen, CloudProvider::Modal);
    assert!((priced.high - estimate.seconds().high * per_second).abs() < 1e-9, "{priced:?}");
    assert!(cost(80.0, 0.0, 3.6, &qwen, CloudProvider::Modal).low > cost(80.0, 0.0, 3.6, &recipe(), CloudProvider::Modal).low,
        "the Qwen worker has more memory");
}

/// A 3.0.0 recipe carries a picked finishing and never the default: its
/// identity does not change until someone picks another. A 2.0.0 recipe
/// (Qwen) carries one, the default too, only when every model is finished.
#[test]
fn the_finishing_setting_rides_on_a_3_0_0_recipe_and_on_others_when_every_model_is_finished() {
    use cleaner_core::engines::postprocess::{Choice, Finishing};
    let picked = serde_json::json!({"cloudFinishing": "dots-only", "cloudFinishingGrayscale": true});
    let choice = Choice { finishing: Finishing::DotsOnly, grayscale: true };
    let setting = finishing_from_settings(&picked).unwrap();
    assert_eq!(setting, FinishingSetting { choice, every_model: false });
    assert_eq!(finishing_from_settings(&serde_json::Value::Null), Ok(FinishingSetting::default()));
    assert!(finishing_from_settings(&serde_json::json!({"cloudFinishing": "blur"})).is_err());
    assert!(finishing_from_settings(&serde_json::json!({"finishingEveryModel": "yes"})).is_err());

    let mut current = RenderRecipe { preprocessing_version: "3.0.0".into(), ..recipe() };
    apply_finishing(&mut current, setting);
    assert_eq!(current.finishing, Some(choice));
    assert!(current.same_deployment(&RenderRecipe { finishing: None, ..current.clone() }));
    let wire: cleaner_core::cloud_wire::WireRenderRecipe = current.clone().into();
    assert!(!serde_json::to_string(&wire).unwrap().contains("raster"), "the worker never sees it");
    apply_finishing(&mut current, FinishingSetting { every_model: true, ..Default::default() });
    assert_eq!(current.finishing, None, "the default is what 3.0.0 does anyway");

    let mut older = RenderRecipe { preprocessing_version: "2.0.0".into(), ..recipe() };
    apply_finishing(&mut older, setting);
    assert_eq!(older.finishing, None, "2.0.0 keeps its tone alignment");
    let every = finishing_from_settings(&serde_json::json!({"finishingEveryModel": true})).unwrap();
    assert_eq!(every.for_others(), Some(Choice::default()));
    apply_finishing(&mut older, every);
    assert_eq!(older.finishing, Some(Choice::default()));
    assert_eq!(FinishingSetting { choice, every_model: false }.for_others(), None);
}

/// A one-region clean's grant is minted for the proposal's recipe, and the
/// render is checked against it in full (`GrantService::validate_and_consume`).
/// The interface sends the proposal's recipe back (`cloudflow.svelte.js`), so
/// the finishing a 2.0.0 recipe carries with every model finished must come
/// back as it went out, and a recipe without it must not pass for it.
#[test]
fn a_2_0_0_recipe_carrying_a_finishing_round_trips_through_the_interface() {
    let mut granted = RenderRecipe { preprocessing_version: "2.0.0".into(), ..recipe() };
    apply_finishing(&mut granted, finishing_from_settings(&serde_json::json!({"finishingEveryModel": true})).unwrap());
    assert!(granted.finishing.is_some());
    let sent: RenderRecipe = serde_json::from_value(serde_json::to_value(&granted).unwrap()).unwrap();
    assert_eq!(sent, granted, "the recipe the interface returns is the one granted");
    let bare = RenderRecipe { finishing: None, ..granted.clone() };
    assert_ne!(bare, granted, "the recipe built in the interface would be refused");
}

/// The generation choice rides on a FLUX 3 recipe only, all-own rides on
/// none, and the one-megapixel size replaces the working size, in the request
/// and in the price.
#[test]
fn the_generation_setting_rides_on_flux3_only_and_megapixel_replaces_the_working_size() {
    use cleaner_core::engines::render::{megapixel_size, Flux3Generation};
    let picked = serde_json::json!({"cloudFlux3Generation": {"marker": true, "megapixelSize": true}});
    let generation = Flux3Generation { marker: true, megapixel_size: true, ..Flux3Generation::default() };
    assert_eq!(flux3_generation_from_settings(&picked), Ok(Some(generation)));
    assert_eq!(flux3_generation_from_settings(&serde_json::json!({"cloudFlux3Generation": {"marker": false}})), Ok(None));
    assert_eq!(flux3_generation_from_settings(&serde_json::Value::Null), Ok(None));
    let every = serde_json::json!({"cloudFlux3Generation": {"locationPrompt": true, "lowShift": true}});
    assert_eq!(flux3_generation_from_settings(&every), Ok(Some(Flux3Generation { location_prompt: true, low_shift: true, ..Flux3Generation::default() })));
    for bad in [serde_json::json!({"marker": "yes"}), serde_json::json!({"colour": true}), serde_json::json!(true)] {
        assert!(flux3_generation_from_settings(&serde_json::json!({"cloudFlux3Generation": bad})).is_err(), "{bad}");
    }
    let flux3 = RenderRecipe::new("mc-flux3-klein-bf16-edit-v2", "3.0.0", "unsloth/flux-3-image-klein",
        "67662613c10c92a1364cb3da8c6f7447977c7ce9", false);
    let gateway = Gateway { recipe: flux3, ..Gateway::priced() };
    let mut sized = deployed_recipe(&gateway, None, Some(1280)).unwrap();
    assert_eq!(sized.work_long_side, Some(1280));
    apply_flux3_generation(&mut sized, Some(generation));
    assert_eq!((sized.work_long_side, sized.flux3_generation), (None, Some(generation)));
    assert_eq!(RenderRate::of(&sized).working_size(300, 200), megapixel_size(300, 200));
    let mut klein = deployed_recipe(&Gateway::priced(), None, None).unwrap();
    apply_flux3_generation(&mut klein, Some(generation));
    assert_eq!(klein.flux3_generation, None);
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
        store.insert(n.to_string(), StoredGrant { scope: scope.clone(), stream: None, expires_at_ms: u64::MAX });
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
    fn render(&self, nonce: &str, page_index: u32, region_id: &str, recipe: &RenderRecipe,
        submitted: &dyn Fn()) -> Result<ApiRegion, String> {
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
        // Accepted remote jobs release the crop/upload slot before waiting.
        // Pre-enqueue failures retain it until the failed attempt returns.
        if !self.fail.contains_key(region_id) { submitted(); }
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
    plan_with(path, ids, grants, chunk_regions, execution, &recipe(), "modal-prof-clean-run", &FINGERPRINT)
}

/// [`plan_for`] against a named recipe, profile and endpoint.
#[allow(clippy::too_many_arguments)]
fn plan_with(path: &Path, ids: &[String], grants: &GrantService, chunk_regions: usize, execution: CleanExecution,
    recipe: &RenderRecipe, profile: &str, endpoint_fingerprint: &str) -> CleanGrantScope {
    let Bound { regions, too_large } = bind(path, ids, recipe_preprocessing(recipe).unwrap()).unwrap();
    assert!(too_large.is_empty());
    let estimate = Estimate::of(&regions, RenderRate::of(recipe));
    let mut scope = CleanGrantScope {
        chapter_id: "chapter".into(),
        regions,
        provider: CloudProvider::Modal,
        profile_id: profile.into(),
        endpoint_fingerprint: endpoint_fingerprint.into(),
        profile_epoch: grants.get_profile_epoch(CloudProvider::Modal, profile),
        recipe: recipe.clone(),
        execution,
        chunk_regions,
        estimate,
        price: l4(),
        cost: Some(cost(estimate.gpu_seconds, estimate.retry_gpu_seconds, 0.8, recipe, CloudProvider::Modal)),
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
    let klein = RenderRecipe::new("mc-flux2-klein-9b-inpaint-v2", "1.0.0",
        "Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32", recipe().model_revision, false);
    let guided = QwenEdit { target: QwenTarget::SoundEffect, description: "Letters beside the hand".into() };
    let auto = QwenEdit { target: QwenTarget::Auto, description: String::new() };
    for (name, deployment, guidance, expected) in [
        ("guided", qwen.clone(), Some(guided.clone()), Some(guided.clone())),
        ("default", qwen, None, Some(auto.clone())),
        ("klein", klein.clone(), Some(guided.clone()), Some(guided.clone())),
        ("klein-default", klein, None, Some(auto)),
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

/// Two regions on each of four pages: every independent first region is
/// accepted before any finishes, never two overlapping regions of one page.
#[test]
fn accepted_jobs_queue_ahead_on_different_pages_and_one_release() {
    const PAGES: usize = MAX_PREPARING + 1;
    let path = chapter("run-bound", PAGES, StripMode::Single);
    let ids: Vec<String> = (0..PAGES).flat_map(|page| [
        detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0),
        detect(&path, page, Rect::new(150, 150, 30, 20), REMOTE_PICK, 1),
    ]).collect();
    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    // More jobs than the preparation budget must be accepted while the GPU
    // has completed none. A render-completion-bound budget deadlocks here.
    let first_page_jobs = Arc::new((Mutex::new(0usize), Condvar::new()));
    renderer.on_render = Box::new(move |_| {
        let (count, changed) = &*first_page_jobs;
        let mut count = count.lock().unwrap();
        *count += 1;
        if *count >= PAGES {
            changed.notify_all();
        } else {
            let (seen, _) = changed.wait_timeout_while(count, Duration::from_secs(10),
                |seen| *seen < PAGES).unwrap();
            assert!(*seen >= PAGES, "jobs waited for a GPU completion before submission");
        }
    });
    renderer.delay = Duration::from_millis(150);
    let (summary, events) = run_with(&path, &ids, &grants, &renderer, &AtomicBool::new(false));

    assert_eq!(renderer.most_in_flight.load(Ordering::SeqCst), PAGES);
    assert!(!renderer.same_page_at_once.load(Ordering::SeqCst), "two regions of one page read each other");
    assert_eq!((summary.reason, summary.regions_cleaned, summary.pages_cleaned, summary.pages_queued),
        ("completed", 8, 4, 4));
    assert_eq!((of_type(&events, "page-started"), of_type(&events, "region-done"), of_type(&events, "page-done")),
        (4, 8, 4));
    assert!(detection_ids(&path).is_empty());
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 1);
}

#[test]
fn queued_jobs_do_not_expand_the_crop_preparation_budget() {
    let path = chapter("run-prepare-bound", MAX_PREPARING + 2, StripMode::Single);
    let ids: Vec<String> = (0..MAX_PREPARING + 2)
        .map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
    let grants = Arc::new(GrantService::new());
    let scope = scope_for(&path, &ids, &grants);
    let renderer = Renderer::new(&path, &grants);
    let (started, starts) = mpsc::channel();
    let resume = (Mutex::new(false), Condvar::new());
    let reading = AtomicUsize::new(0);
    let most_reading = AtomicUsize::new(0);
    let reread = |path: &Path, page: u32, id: &str, recipe: &RenderRecipe| {
        let count = reading.fetch_add(1, Ordering::SeqCst) + 1;
        most_reading.fetch_max(count, Ordering::SeqCst);
        let _ = started.send(());
        let (ready, changed) = &resume;
        let ready = ready.lock().unwrap();
        drop(changed.wait_while(ready, |ready| !*ready).unwrap());
        let input = read_render_input(path, page, id, recipe);
        reading.fetch_sub(1, Ordering::SeqCst);
        input
    };
    let (summary, _) = std::thread::scope(|threads| {
        let resume = &resume;
        threads.spawn(move || {
            for _ in 0..MAX_PREPARING {
                starts.recv_timeout(Duration::from_secs(10)).expect("the preparation budget fills");
            }
            let extra = starts.recv_timeout(Duration::from_millis(100));
            // Release even on failure so the scoped workers can finish.
            *resume.0.lock().unwrap() = true;
            resume.1.notify_all();
            assert!(matches!(extra, Err(mpsc::RecvTimeoutError::Timeout)),
                "a fourth crop started while all preparation slots were occupied");
        });
        run_scope(&scope, &path, &grants, &renderer, &AtomicBool::new(false), &Harness { read: &reread, ..OPEN })
    });
    assert_eq!(most_reading.load(Ordering::SeqCst), MAX_PREPARING);
    assert_eq!(summary.regions_cleaned as usize, ids.len());
}

/// A gateway of many jobs. It counts the jobs it holds between a submission
/// and the download of the result, and keeps every job running until
/// [`QueueState::finish`] is set.
struct QueueGateway {
    endpoint: String,
    state: Arc<QueueState>,
}

#[derive(Default)]
struct QueueState {
    /// Each accepted job's request, by handle.
    jobs: Mutex<HashMap<String, serde_json::Value>>,
    /// Jobs sent whose result is not downloaded yet.
    open: Mutex<HashSet<String>>,
    accepted: AtomicUsize,
    most_open: AtomicUsize,
    finish: AtomicBool,
}

impl QueueGateway {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://127.0.0.1:{}/mc/v1", listener.local_addr().unwrap().port());
        let state = Arc::new(QueueState::default());
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let state = Arc::clone(&shared);
                std::thread::spawn(move || state.serve(&mut stream));
            }
        });
        Self { endpoint, state }
    }

    fn client(&self, profile: &str) -> CloudHttpClient {
        use crate::inference::http::{BoundRuntimeCredential, CloudEndpointTarget, RuntimeCredential};
        let target = CloudEndpointTarget::new_test_target(CloudProvider::Modal, profile, &self.endpoint).unwrap();
        let credential = BoundRuntimeCredential::new(&target, RuntimeCredential::ModalProxy {
            token_id: "tid".into(),
            token_secret: crate::inference::secrets::SecretValue::new("tsec"),
        }).unwrap();
        CloudHttpClient::new_test_client(target, credential,
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap())
    }

    fn open(&self) -> usize { self.state.open.lock().unwrap().len() }
    fn accepted(&self) -> usize { self.state.accepted.load(Ordering::SeqCst) }
}

impl QueueState {
    fn serve(&self, stream: &mut std::net::TcpStream) {
        use crate::inference::service::tests as wire;
        let Some(request) = wire::read_fake_request(stream) else { return };
        let answer = |stream: &mut std::net::TcpStream, status: &str, body: serde_json::Value| {
            wire::fake_respond(stream, status, "application/json", body.to_string().as_bytes())
        };
        let known = |handle: &str| self.jobs.lock().unwrap().get(handle).cloned();
        let parts: Vec<&str> = request.path.trim_start_matches("/mc/v1/").split('/').collect();
        match (request.method.as_str(), parts.as_slice()) {
            ("GET", ["capabilities"]) => answer(stream, "200 OK", serde_json::json!({
                "protocol_version": "1.0.0", "provider": "modal",
                "operations": ["jobs.submit", "jobs.status", "jobs.result", "jobs.cancel"],
            })),
            ("POST", ["jobs"]) => {
                let meta = wire::submission_metadata(&request.body);
                let handle = format!("h-{}", meta["attempt_id"].as_str().unwrap());
                let mut body = wire::fake_job_identity(&meta);
                body["handle"] = handle.clone().into();
                body["status"] = "pending".into();
                self.jobs.lock().unwrap().insert(handle.clone(), meta);
                {
                    let mut open = self.open.lock().unwrap();
                    open.insert(handle);
                    self.most_open.fetch_max(open.len(), Ordering::SeqCst);
                }
                self.accepted.fetch_add(1, Ordering::SeqCst);
                answer(stream, "202 Accepted", body);
            }
            ("GET", ["jobs", handle]) => {
                let Some(meta) = known(handle) else { return answer(stream, "404 Not Found", serde_json::json!({})) };
                let mut body = wire::fake_job_identity(&meta);
                body["handle"] = (*handle).into();
                body["status"] = "running".into();
                if self.finish.load(Ordering::SeqCst) {
                    let png = wire::fake_result_png(&meta);
                    body["status"] = "completed".into();
                    body["result_digest"] = sha256_hex(&png).into();
                    body["result_bytes"] = png.len().into();
                }
                answer(stream, "200 OK", body);
            }
            ("GET", ["jobs", handle, "result"]) => {
                let Some(meta) = known(handle) else { return answer(stream, "404 Not Found", serde_json::json!({})) };
                self.open.lock().unwrap().remove(*handle);
                wire::fake_respond(stream, "200 OK", "image/png", &wire::fake_result_png(&meta));
            }
            _ => answer(stream, "404 Not Found", serde_json::json!({})),
        }
    }
}

/// The app's own render path ([`ServiceRenderer`]) without the app.
struct ServiceBacked<'a> {
    service: &'a InferenceService,
    job_path: &'a Path,
    target: ExecutionTarget,
    config: &'a InferenceConfig,
    deployed: RenderRecipe,
}

impl RegionRenderer for ServiceBacked<'_> {
    fn render(&self, nonce: &str, page_index: u32, region_id: &str, recipe: &RenderRecipe,
        submitted: &dyn Fn()) -> Result<ApiRegion, String> {
        let poll = PollOptions { poll_interval: Duration::from_millis(20), timeout: Duration::from_secs(60),
            max_polls: 3000 };
        self.service.execute_cloud_render_queued(nonce, self.job_path, page_index, region_id, &self.target,
            recipe, &OperationIntent::CloudClean, self.config, true, &poll, submitted)
            .map_err(|e| e.code().to_owned())
    }

    fn deployment(&self) -> Result<(RenderRecipe, Option<(String, f64)>), String> {
        Ok((self.deployed.clone(), l4()))
    }

    fn release_idle(&self) {}
}

/// Wait, bounded, until `reached` holds. Answers whether it did.
fn wait_until(limit: Duration, reached: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while !reached() {
        if std::time::Instant::now() >= deadline { return false; }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

/// The queue keeps filling while regions are left (docs/fixes-2026-10-12/04).
/// Forty Qwen regions on forty pages go through the app's own render path.
/// The gateway holds every job until [`MAX_IN_FLIGHT`] are open, so the bound
/// is met and seen not to be passed. Then all forty are sent and committed.
/// Nobody is asked about a result: while a person was, sixteen results that
/// waited for an answer held every worker and nothing more was sent.
#[test]
fn forty_qwen_regions_are_all_sent_and_committed_with_sixteen_at_most_in_flight() {
    use cleaner_core::engines::render::{QwenEdit, QwenTarget};
    use crate::inference::config::{compute_canonical_endpoint_fingerprint, validate_https_endpoint, CloudProfile};
    use crate::inference::consent::ConsentService;
    use crate::inference::journal::AttemptPhase;
    use crate::inference::secrets::SecretManager;
    const PAGES: usize = 40;
    const PROFILE: &str = "modal-prof-queue";
    const URL: &str = "https://modal.run/mc/v1";

    let path = chapter("qwen-queue", PAGES, StripMode::Single);
    let ids: Vec<String> = (0..PAGES)
        .map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
    let mut qwen = RenderRecipe::new("mc-qwen-image-edit-2511-v4", "1.0.0",
        "Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32", recipe().model_revision, false);
    qwen.qwen_edit = Some(QwenEdit { target: QwenTarget::Auto, description: String::new() });

    let mut config = InferenceConfig::default();
    let (origin, origin_fingerprint) = validate_https_endpoint(URL).unwrap();
    config.modal_profiles.insert(PROFILE.into(), CloudProfile {
        id: PROFILE.into(), name: "Queue".into(), endpoint_url: URL.into(), canonical_origin: origin,
        canonical_origin_fingerprint: origin_fingerprint, created_at_ms: 100, updated_at_ms: 100,
    });
    let grants = Arc::new(GrantService::new());
    let scope = plan_with(&path, &ids, &grants, CHUNK_REGIONS, CleanExecution::Cloud, &qwen, PROFILE,
        &compute_canonical_endpoint_fingerprint(URL).unwrap());

    let gateway = QueueGateway::start();
    let service = InferenceService::new_isolated(
        path.parent().unwrap().parent().unwrap().join("journal"),
        cleaner_core::cloud_wire::provisional_fixture_limits(),
        Arc::new(ConsentService::new_isolated(Arc::clone(&grants))), Arc::clone(&grants),
        Arc::new(SecretManager::new_in_memory()),
    ).with_custom_client(gateway.client(PROFILE));
    let renderer = ServiceBacked {
        service: &service, job_path: &path, target: ExecutionTarget::Modal { profile_id: PROFILE.into() },
        config: &config, deployed: qwen.clone(),
    };
    let batch = Batch {
        scope: &scope, job_path: &path, grants: &grants, allowed: OPEN.allowed, unresolved: OPEN.unresolved,
        local_pass: None, read: OPEN.read,
    };

    let events = Mutex::new(Vec::new());
    let (summary, held) = std::thread::scope(|threads| {
        let watch = threads.spawn(|| {
            // Every job stays running until the bound is met, and a little
            // longer, so a seventeenth request would be seen.
            let full = wait_until(Duration::from_secs(20), || gateway.open() >= MAX_IN_FLIGHT);
            std::thread::sleep(Duration::from_millis(300));
            let held = (full, gateway.accepted());
            gateway.state.finish.store(true, Ordering::SeqCst);
            held
        });
        let summary = run_plan(&batch, &renderer, "run-test", &AtomicBool::new(false),
            &|event| events.lock().unwrap().push(event));
        (summary, watch.join().unwrap())
    });

    assert_eq!(held, (true, MAX_IN_FLIGHT), "the bound is met, and no request passes it");
    // Each result is on the page with the flag the user finds it by.
    let flagged = events.into_inner().unwrap().into_iter().filter(|event| matches!(event,
        Event::RegionDone { region, .. } if region.mask.as_ref().is_some_and(|mask| mask.generated_texture_review)))
        .count();
    assert_eq!(flagged, PAGES);
    assert_eq!(gateway.state.most_open.load(Ordering::SeqCst), MAX_IN_FLIGHT);
    assert_eq!((summary.reason, summary.regions_cleaned as usize, summary.errored), ("completed", PAGES, 0));
    assert_eq!(gateway.accepted(), PAGES);
    assert!(detection_ids(&path).is_empty());
    let attempts = service.journal().list_attempt_ids().unwrap();
    assert_eq!(attempts.len(), PAGES);
    assert!(attempts.iter().all(|id| matches!(service.journal().get_record(id).unwrap().phase,
        AttemptPhase::Committed { .. })));
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
    // Parallel workers: notices come in the order the regions finish.
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
    let path = chapter("run-cancel", MAX_IN_FLIGHT + 2, StripMode::Single);
    let ids: Vec<String> = (0..MAX_IN_FLIGHT + 2)
        .map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
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
        work_long_side: None,
        flux3_generation: None,
        finishing: None,
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

/// A request that will never finish does not hold its region: the sweep that
/// runs before a cloud clean is prepared records it failed, and the plan made
/// next cleans the region. Here the result arrived and its cached file is
/// gone, so it can never be applied.
#[test]
fn a_region_whose_attempt_the_sweep_ended_is_planned_by_the_next_prepare() {
    use crate::inference::journal::AttemptPhase;
    let (path, ids) = three_chunks("journal-swept");
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
        work_long_side: None,
        flux3_generation: None,
        finishing: None,
    };
    let (_, guard) = journal.create_intent(intent).unwrap();
    journal.mark_dispatching(&guard).unwrap();
    journal.record_accepted_handle(&guard, "handle-swept").unwrap();
    drop(guard);
    let mut record = journal.get_record(&request.attempt_id).unwrap();
    record.phase = AttemptPhase::ResultCached {
        handle: "handle-swept".into(), result_digest: "a".repeat(64), cached_png_bytes: 64, reported_cost_usd: None,
    };
    std::fs::write(root.join("attempts").join(&request.attempt_id).join("journal.json"),
        serde_json::to_vec(&record).unwrap()).unwrap();

    let unresolved = journal_unresolved(journal.clone(), &path);
    let plan = || prepare(&path, asked("chapter", false), destination("modal-prof-clean-journal"),
        &Gateway::priced(), &unresolved).unwrap();
    let held = plan();
    assert_eq!(held.unresolved_ids, vec![ids[2].clone()]);
    assert!(!held.region_ids.contains(&ids[2]));

    let service = crate::inference::service::InferenceService::new(&root, cleaner_core::cloud_wire::provisional_fixture_limits());
    let (report, waits) = crate::inference::commands::end_unresolved_attempts(&service, &|_| Some(path.clone()));
    assert_eq!((report.released.len(), report.needs_attention.len(), waits.len()), (1, 0, 0));
    assert!(matches!(journal.get_record(&request.attempt_id).unwrap().phase, AttemptPhase::Failed { .. }));

    let freed = plan();
    assert!(freed.unresolved_ids.is_empty());
    assert!(freed.region_ids.contains(&ids[2]), "the region is planned again");
}

/* ------------------------------------------------------------------ */
/* Streaming                                                           */
/* ------------------------------------------------------------------ */

fn streamed(scope: &str, pages: Option<Vec<u32>>) -> StreamRequest {
    StreamRequest {
        chapter_id: "chapter".into(), scope: scope.into(), page_indices: pages, local_first: false, picks: None,
        qwen_edit: None,
        work_long_side: None,
        flux3_generation: None,
        finishing: Default::default(),
    }
}

/// The scope a spent streaming grant holds: the destination and price, no regions.
fn stream_scope(grants: &GrantService, profile: &str) -> CleanGrantScope {
    CleanGrantScope {
        chapter_id: "chapter".into(),
        regions: Vec::new(),
        provider: CloudProvider::Modal,
        profile_id: profile.into(),
        endpoint_fingerprint: FINGERPRINT.clone(),
        profile_epoch: grants.get_profile_epoch(CloudProvider::Modal, profile),
        recipe: recipe(),
        execution: CleanExecution::Cloud,
        chunk_regions: CHUNK_REGIONS,
        estimate: Estimate::default(),
        price: l4(),
        cost: None,
        plan_digest: "s".repeat(64),
    }
}

fn terms(pages: u32, cap_usd: f64) -> StreamTerms {
    StreamTerms { pages: (0..pages).collect(), cap_usd, picks: None }
}

/// Run a stream whose pages `feed` hands on, from a thread of its own, as a
/// detection would: it stores each page's detections, then sends the page.
#[allow(clippy::too_many_arguments)]
fn run_streamed(scope: &CleanGrantScope, terms: &StreamTerms, path: &Path, grants: &Arc<GrantService>,
    renderer: &Renderer, cancel: &AtomicBool, harness: &Harness<'_>,
    feed: impl FnOnce(&mpsc::Sender<(u32, bool)>) + Send) -> (Summary, Vec<Event>) {
    let batch = Batch {
        scope, job_path: path, grants, allowed: harness.allowed, unresolved: harness.unresolved,
        local_pass: harness.local_pass, read: harness.read,
    };
    let events = RefCell::new(Vec::new());
    let (tx, rx) = mpsc::channel();
    let summary = std::thread::scope(|threads| {
        threads.spawn(move || feed(&tx));
        run_stream(&batch, renderer, terms, "run-stream", &rx, &|item: &(u32, bool)| *item, cancel,
            &|event| events.borrow_mut().push(event))
    });
    (summary, events.into_inner())
}

fn wait_for(what: &str, reached: impl Fn() -> bool) {
    assert!(wait_until(Duration::from_secs(10), reached), "{what}");
}

/// A page's regions go to the cloud GPU as soon as the page is handed on:
/// the feed does not detect page 2 until page 1's region has rendered, which
/// a clean that waited for detection to end would never let happen. Every
/// region is cleaned, each page finished once, the GPU released once.
#[test]
fn a_stream_cleans_each_page_while_later_pages_are_still_detecting() {
    let path = chapter("stream-overlap", 3, StripMode::Single);
    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    renderer.delay = Duration::from_millis(5);
    let rendered = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&rendered);
    renderer.on_render = Box::new(move |id| seen.lock().unwrap().push(id.to_owned()));
    let scope = stream_scope(&grants, "modal-prof-stream-overlap");
    let feed_path = path.clone();
    let (summary, events) = run_streamed(&scope, &terms(3, MAX_STREAM_CAP_USD), &path, &grants, &renderer,
        &AtomicBool::new(false), &OPEN, |tx| {
            for page in 0..3usize {
                let id = {
                    // Under the job's lock, as a detection writes, since renders commit meanwhile.
                    let _lock = run::lock_job(&feed_path).unwrap();
                    detect(&feed_path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)
                };
                tx.send((page as u32, true)).unwrap();
                wait_for("the handed page was rendered before detection went on",
                    || rendered.lock().unwrap().contains(&id));
            }
        });
    assert_eq!((summary.reason, summary.regions_cleaned, summary.pages_cleaned, summary.next_page_index),
        ("completed", 3, 3, None));
    // Detection announced each page; the stream finishes each once.
    assert_eq!((of_type(&events, "page-started"), of_type(&events, "page-done")), (0, 3));
    assert!(detection_ids(&path).is_empty());
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 1);
    // The deployment is checked again before each page, as before each chunk.
    assert_eq!(renderer.rendered_at_deployment.lock().unwrap().len(), 3);
}

/// The cap counts each region at its high bound as it is queued, the fixed
/// cost with the first. The region that would pass it is not queued, nor any
/// after it: they stay detected, said once, and detection's later pages are
/// finished as they come. Nothing past the cap reaches the renderer.
#[test]
fn the_cap_stops_queueing_and_leaves_the_rest_detected() {
    let path = chapter("stream-cap", 3, StripMode::Single);
    let ids: Vec<String> = (0..3).flat_map(|page| [
        detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0),
        detect(&path, page, Rect::new(150, 150, 30, 20), REMOTE_PICK, 1),
    ]).collect();
    let Bound { regions, .. } = bind(&path, &ids, recipe_preprocessing(&recipe()).unwrap()).unwrap();
    let budget = Budget::new(&recipe(), CloudProvider::Modal, 0.8, 0.0);
    let three = budget.fixed_usd + regions[..3].iter().map(|region| budget.job_usd(region.crop)).sum::<f64>();
    let cap = three + budget.job_usd(regions[3].crop) / 2.0;

    let grants = Arc::new(GrantService::new());
    let renderer = Renderer::new(&path, &grants);
    let scope = stream_scope(&grants, "modal-prof-stream-cap");
    let (summary, events) = run_streamed(&scope, &terms(3, cap), &path, &grants, &renderer,
        &AtomicBool::new(false), &OPEN, |tx| {
            for page in 0..3 { tx.send((page, true)).unwrap(); }
        });
    let mut rendered = renderer.rendered();
    rendered.sort();
    let mut first = ids[..3].to_vec();
    first.sort();
    assert_eq!(rendered, first);
    assert_eq!(detection_ids(&path), ids[3..].to_vec());
    let capped = notices(&events, "notice.cloudClean.capReached");
    assert_eq!((capped.len(), capped[0]["page"].as_u64()), (1, Some(2)));
    assert_eq!((summary.reason, summary.regions_cleaned, summary.next_page_index), ("completed", 3, None));
    assert_eq!(of_type(&events, "page-done"), 3);
    // Only the pages before the cap were checked against the deployment.
    assert_eq!(renderer.rendered_at_deployment.lock().unwrap().len(), 2);
}

/// A deployment whose GPU or price moved stops the stream at the next page,
/// as it stops a plan at the next chunk: what was queued finishes, nothing
/// more is sent, and the pages detection hands on after are left detected.
#[test]
fn a_changed_price_stops_the_stream_at_the_next_page() {
    let path = chapter("stream-price", 3, StripMode::Single);
    let ids: Vec<String> = (0..3).map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    renderer.deployed = vec![(recipe(), l4()), (recipe(), Some(("L4".into(), 1.6)))];
    let scope = stream_scope(&grants, "modal-prof-stream-price");
    let (summary, events) = run_streamed(&scope, &terms(3, MAX_STREAM_CAP_USD), &path, &grants, &renderer,
        &AtomicBool::new(false), &OPEN, |tx| {
            for page in 0..3 { tx.send((page, true)).unwrap(); }
        });
    assert_eq!(renderer.rendered(), vec![ids[0].clone()]);
    assert_eq!(detection_ids(&path), ids[1..].to_vec());
    let stopped = notices(&events, "notice.cloudClean.stopped");
    assert_eq!((stopped.len(), stopped[0]["code"].as_str()), (1, Some("cloud_clean_gpu_changed")));
    assert_eq!((summary.reason, summary.regions_cleaned), ("completed", 1));
    assert_eq!(of_type(&events, "page-done"), 3);
    // A changed deployment is not a lost authority: the GPU it woke is released.
    assert_eq!(renderer.releases.load(Ordering::SeqCst), 1);
}

/// A cancel stops the renders in flight and every page handed on after it.
/// The first page not finished is where a resume picks up, and every region
/// stays detected.
#[test]
fn a_cancel_mid_stream_leaves_the_handed_pages_for_a_resume() {
    let path = chapter("stream-cancel", 3, StripMode::Single);
    let ids: Vec<String> = (0..3).map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
    let grants = Arc::new(GrantService::new());
    let mut renderer = Renderer::new(&path, &grants);
    renderer.hold = true;
    let cancel = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&started);
    renderer.on_render = Box::new(move |_| { seen.fetch_add(1, Ordering::SeqCst); });
    let scope = stream_scope(&grants, "modal-prof-stream-cancel");
    let stop = Arc::clone(&cancel);
    let (summary, events) = run_streamed(&scope, &terms(3, MAX_STREAM_CAP_USD), &path, &grants, &renderer,
        &cancel, &OPEN, |tx| {
            tx.send((0, true)).unwrap();
            wait_for("page 1's render started", || started.load(Ordering::SeqCst) == 1);
            stop.store(true, Ordering::SeqCst);
            tx.send((1, true)).unwrap();
            tx.send((2, false)).unwrap();
        });
    assert_eq!((summary.reason, summary.regions_cleaned, summary.next_page_index), ("cancelled", 0, Some(0)));
    assert_eq!(detection_ids(&path), ids);
    assert_eq!(renderer.rendered(), vec![ids[0].clone()]);
    // Page 3 was not detected by the run, so nothing announced it to finish.
    assert_eq!(of_type(&events, "page-done"), 2);
}

/// A region with an unresolved earlier request is not sent, with the notice a
/// plan gives it; a page the consent did not name sends nothing at all.
#[test]
fn a_stream_skips_unresolved_regions_and_pages_it_was_not_given() {
    let path = chapter("stream-skips", 3, StripMode::Single);
    let ids: Vec<String> = (0..3).map(|page| detect(&path, page, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0)).collect();
    let held = ids[0].clone();
    let unresolved = move |_: u32, id: &str| id == held;
    let grants = Arc::new(GrantService::new());
    let renderer = Renderer::new(&path, &grants);
    let scope = stream_scope(&grants, "modal-prof-stream-skips");
    let given = StreamTerms { pages: [0, 1].into(), cap_usd: MAX_STREAM_CAP_USD, picks: None };
    let (summary, events) = run_streamed(&scope, &given, &path, &grants, &renderer, &AtomicBool::new(false),
        &Harness { unresolved: &unresolved, ..OPEN }, |tx| {
            for page in 0..3 { tx.send((page, true)).unwrap(); }
        });
    assert_eq!(renderer.rendered(), vec![ids[1].clone()]);
    let skipped = notices(&events, "notice.cloudClean.regionSkipped");
    assert_eq!((skipped.len(), skipped[0]["reason"].as_str()), (1, Some("unresolved")));
    assert_eq!(summary.regions_cleaned, 1);
    assert_eq!(detection_ids(&path), vec![ids[0].clone(), ids[2].clone()]);
}

/// Streaming consent: a proposal needs a stated price; its digest binds the
/// pages; the cap is answered at confirm, within the fixed cost and the
/// maximum; and a stream's proposal and grant are never a plan's, nor the
/// reverse.
#[test]
fn a_stream_is_consented_with_a_cap_and_never_spent_as_a_plan() {
    let path = chapter("stream-consent", 3, StripMode::Single);
    let profile = "modal-prof-stream-consent";
    let unpriced = Gateway { price: None, ..Gateway::priced() };
    assert_eq!(prepare_stream(&path, streamed("chapter", None), destination(profile), &unpriced).unwrap_err(),
        "cloud_clean_stream_unpriced");
    assert_eq!(prepare_stream(&path, streamed("page", Some(vec![3])), destination(profile), &Gateway::priced())
        .unwrap_err(), "cloud_clean_page_not_found");
    assert_eq!(prepare_stream(&path, streamed("regions", None), destination(profile), &Gateway::priced())
        .unwrap_err(), "cloud_clean_scope_unsupported");

    let proposal = prepare_stream(&path, streamed("chapter", None), destination(profile), &Gateway::priced()).unwrap();
    assert_eq!((proposal.pages, proposal.page_indices.clone(), proposal.gpu.as_str()), (3, vec![0, 1, 2], "L4"));
    assert!(proposal.fixed_cost_usd > 0.0 && proposal.region_cost_usd > 0.0);
    assert!(proposal.suggested_cap_usd >= proposal.fixed_cost_usd && proposal.suggested_cap_usd <= MAX_STREAM_CAP_USD);
    let one = prepare_stream(&path, streamed("page", Some(vec![1])), destination(profile), &Gateway::priced()).unwrap();
    assert_ne!(one.stream_digest, proposal.stream_digest, "the digest binds the pages");
    assert!(discard(&one.proposal_id).unwrap());
    assert!(Job::open(&path).unwrap().project.detections.is_empty(), "preparing writes nothing");

    let id = proposal.proposal_id.clone();
    // A plan's confirm does not take a stream, and spends it.
    assert_eq!(confirm(&id, &proposal.stream_digest, true, true).unwrap_err(), "cloud_clean_plan_mismatch");
    for cap in [proposal.fixed_cost_usd / 2.0, MAX_STREAM_CAP_USD + 1.0, f64::NAN] {
        let again = prepare_stream(&path, streamed("chapter", None), destination(profile), &Gateway::priced()).unwrap();
        assert_eq!(confirm_stream(&again.proposal_id, &again.stream_digest, cap, true, true).unwrap_err(),
            "cloud_clean_cap_invalid");
    }
    let again = prepare_stream(&path, streamed("chapter", None), destination(profile), &Gateway::priced()).unwrap();
    assert_eq!(confirm_stream(&again.proposal_id, &again.stream_digest, 1.0, false, true).unwrap_err(),
        "rights_attestation_required");
    let grant = confirm_stream(&again.proposal_id, &again.stream_digest, 1.0, true, true).unwrap();
    assert_eq!(consume_grant(&grant.grant_id, &request(profile, &recipe())).unwrap_err(),
        "cloud_clean_grant_mismatch: stream");
    let again = prepare_stream(&path, streamed("chapter", None), destination(profile), &Gateway::priced()).unwrap();
    let grant = confirm_stream(&again.proposal_id, &again.stream_digest, 1.0, true, true).unwrap();
    let (scope, terms) = consume_stream_grant(&grant.grant_id, &request(profile, &recipe())).unwrap();
    assert_eq!((terms.pages, terms.cap_usd, scope.regions.len()), ([0, 1, 2].into(), 1.0, 0));
    assert_eq!(scope.plan_digest, again.stream_digest);

    // A plan's grant is not a stream's.
    detect(&path, 0, Rect::new(40, 40, 30, 20), REMOTE_PICK, 0);
    let plan = proposed(&path, profile);
    let grant = agree(&plan, true, true).unwrap();
    assert_eq!(consume_stream_grant(&grant.grant_id, &request(profile, &recipe())).unwrap_err(),
        "cloud_clean_grant_mismatch: stream");
}

/// FLUX 3 v2 edits a crop a second time when the lettering stays (5 of 19 real crops), and
/// that second edit bills GPU time, so the high end of the estimate counts it. A marker
/// request has its own check and no second edit.
#[test]
fn the_high_estimate_counts_the_second_flux3_edit() {
    let flux3 = RenderRecipe::new("mc-flux3-klein-edit-v2", "3.0.0", "OzzyGT/Flux_3_Image_Klein_sdnq_dynamic_4bit",
        "bf0af016a936bfe422eec96557aee058c3a6174e", false);
    let job = Estimate::job(Rect::new(0, 0, 900, 700), RenderRate::of(&flux3));
    assert!(job.retry_gpu_seconds > 0.0, "{job:?}");
    let marked = RenderRecipe { flux3_generation: Some(cleaner_core::engines::render::Flux3Generation { marker: true, ..Default::default() }), ..flux3 };
    assert_eq!(Estimate::job(Rect::new(0, 0, 900, 700), RenderRate::of(&marked)).retry_gpu_seconds, 0.0);
}

/// A profile still on FLUX 3 v1 after the app moved to v2 is told to update, as Qwen's old
/// recipe is, and no plan is made under it.
#[test]
fn an_outdated_flux3_profile_is_told_to_update() {
    let old = RenderRecipe::new("mc-flux3-klein-edit-v1", "3.0.0", "OzzyGT/Flux_3_Image_Klein_sdnq_dynamic_4bit",
        "bf0af016a936bfe422eec96557aee058c3a6174e", false);
    let gateway = Gateway { recipe: old, ..Gateway::priced() };
    assert_eq!(deployed_recipe(&gateway, None, Some(1280)).unwrap_err(), "flux3_update_required");
}
