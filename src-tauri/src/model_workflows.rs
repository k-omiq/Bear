//! Independent model capabilities for review-only page analysis. The existing
//! CTD cleaning run is untouched. No analysis result is an erase permission.
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::library::Library;
use base64::Engine;
use cleaner_core::accel::{Accelerator, Preference};
use cleaner_core::balloon::BalloonDetector;
use cleaner_core::detect::Detector;
use cleaner_core::fusion::ReviewEvidence;
use cleaner_core::image::Raster;
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::patch::{Engine as PatchEngine, Patch, Provenance};
use cleaner_core::project::{Job, StripMode};
use cleaner_core::rt_regions::FullRegions;
use cleaner_core::sam_ts;
use cleaner_core::text_groups::{EvidenceModel, Execution, ModelUse, Seam, SpatialInput};
use cleaner_core::text_shape::{
    MASK_PLAN_VERSION, MaskPlan, MaskQualityState, MaskRaster, PreparedMaskPlan,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{Emitter, Manager};

struct Graph {
    name: &'static str,
    bytes: u64,
    sha256: &'static str,
}

const GRAPHS: [Graph; 2] = [
    Graph {
        name: sam_ts::ENCODER,
        bytes: 1_335_305_985,
        sha256: "9b3a32f9018008cfd2c7a5b1a7eb6e20822ba43eab58918863f74ac62ecbafbe",
    },
    Graph {
        name: sam_ts::HEAD,
        bytes: 22_704_641,
        sha256: "a2c63ccf54e2e692a281cffd4dcda648f252ae6649dc7d23d0203e5868685281",
    },
];
const SAM_REVISION: &str = cleaner_core::text_groups::SAM_TS_L_REVISION;
const RT_SHA256: &str = "5fe9e4f576e49d4e7e8b0e029d6d3cdc252abd4694113e1cae120e62c931ea79";
const RT_BYTES: u64 = 11_120_765;
const FULL_RT_NAME: &str = "detector.onnx";
const FULL_RT_BYTES: u64 = 168_481_531;
const FULL_RT_SHA256: &str = "065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195";
const FULL_RT_REVISION: &str = cleaner_core::text_groups::OGKALU_FULL_REVISION;
// Measured native CPU chapter peak RSS was 8,547,975,168 bytes on Apple M5. Leave room
// for the editor and decoded page; an unknown reading fails closed.
const SAM_MIN_ROOM_BYTES: u64 = 10_000_000_000;
static SAM_FILES: Mutex<()> = Mutex::new(());
static SAM_INSTALL: Mutex<()> = Mutex::new(());

const WRITE_SUPPORT_VERSION: &str = "mask-plan-support-v1";
const RENDER_VERSION: &str = "bounded-ring-median-v1";
// The native 45-page parity result qualifies only this measured host and the
// exact app runtime. Other hosts and CPU remain review-only.
const SAM_WEBGPU_RUNTIME_SHA256: &str =
    "dc19bbcb2f5c9fb3c68b4f9248aa0a35065ff702c5dbeae75eac54a74da97b6d";
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

pub(crate) fn source_mime(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.starts_with(PNG_MAGIC) {
        Ok("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Ok("image/webp")
    } else if bytes.starts_with(b"BM") {
        Ok("image/bmp")
    } else {
        Err("Source preview format is unsupported".into())
    }
}

struct StoredAnalysis {
    id: String,
    sam_backend: &'static str,
    write_eligible: bool,
    runtime_sha256: Option<&'static str>,
    source_sha256: String,
    model_sha256: String,
    width: u32,
    height: u32,
    mask: Vec<u8>,
    bubble_associated_components: std::collections::HashSet<String>,
    candidate_bounds: HashMap<String, Rect>,
}

struct PreparedComponent {
    id: String,
    analysis_id: String,
    sam_backend: &'static str,
    runtime_sha256: &'static str,
    chapter_id: String,
    page_index: usize,
    component_id: String,
    source_sha256: String,
    model_sha256: String,
    support_sha256: String,
    underlay_sha256: String,
    predecessors_sha256: String,
    order: u32,
    support: Mask,
    mask_plan: MaskPlan,
    prepared_mask_plan: PreparedMaskPlan,
}

#[derive(Default)]
struct ReviewStore {
    analysis: Option<StoredAnalysis>,
    prepared: Option<PreparedComponent>,
}

fn review_store() -> &'static Mutex<ReviewStore> {
    static STORE: OnceLock<Mutex<ReviewStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(ReviewStore::default()))
}

fn active_analyses() -> &'static Mutex<HashMap<String, Option<Arc<sam_ts::Cancellation>>>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, Option<Arc<sam_ts::Cancellation>>>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

struct ActiveAnalysis(String);
impl Drop for ActiveAnalysis {
    fn drop(&mut self) {
        if let Some(entry) = active_analyses().lock().unwrap_or_else(|e| e.into_inner()).get_mut(&self.0) {
            *entry = None;
        }
    }
}

fn begin_analysis(request_id: String) -> Result<(ActiveAnalysis, Arc<sam_ts::Cancellation>), String> {
    if request_id.is_empty() {
        return Err("Analysis request id is required".into());
    }
    let mut active = active_analyses().lock().unwrap_or_else(|e| e.into_inner());
    if active.contains_key(&request_id) {
        return Err("Analysis request id was already used".into());
    }
    let cancel = Arc::new(sam_ts::Cancellation::default());
    active.insert(request_id.clone(), Some(cancel.clone()));
    Ok((ActiveAnalysis(request_id), cancel))
}

#[tauri::command]
pub fn cancel_capability_analysis(request_id: String) -> Result<bool, String> {
    let active = active_analyses().lock().map_err(|e| e.to_string())?;
    let Some(cancel) = active.get(&request_id).and_then(Option::as_ref) else { return Ok(false) };
    Ok(cancel.cancel())
}

fn commit_analysis(_active_guard: &ActiveAnalysis, cancel: &sam_ts::Cancellation, analysis: StoredAnalysis) -> Result<(), String> {
    let _active = active_analyses().lock().map_err(|e| e.to_string())?;
    cancel.check()?;
    let mut store = review_store().lock().map_err(|e| e.to_string())?;
    store.analysis = Some(analysis);
    store.prepared = None;
    cancel.finish();
    Ok(())
}

fn detector_bounds(evidence: &ReviewEvidence, component_id: &str) -> Option<Rect> {
    evidence.links.iter()
        .filter(|link| link.component_id == component_id)
        .filter_map(|link| evidence.regions.iter()
            .find(|region| region.id == link.region_id && region.id.starts_with("rt-"))
            .map(|region| (region, link.shared_pixels)))
        .max_by_key(|(region, shared)| (region.kind != "bubble_context", *shared))
        .map(|(region, _)| region.bounds)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Workflow { ctd: bool, rt: bool, sam: bool }
impl Workflow {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "regions" => Ok(Self { ctd: false, rt: true, sam: false }),
            "mask" => Ok(Self { ctd: false, rt: false, sam: true }),
            "text_shape" => Ok(Self { ctd: false, rt: true, sam: true }),
            "ctd" => Ok(Self { ctd: true, rt: false, sam: false }),
            "ctd_regions" => Ok(Self { ctd: true, rt: true, sam: false }),
            "ctd_mask" => Ok(Self { ctd: true, rt: false, sam: true }),
            "ctd_text_shape" => Ok(Self { ctd: true, rt: true, sam: true }),
            _ => Err("Choose a supported detection model combination".into()),
        }
    }
    fn ctd(self) -> bool { self.ctd }
    fn rt(self) -> bool { self.rt }
    fn sam(self) -> bool { self.sam }
}

fn digest(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut chunk = [0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(PartialEq, Eq)]
struct StatusFileStamp {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

#[derive(Default)]
struct StatusDigestCache(Vec<(StatusFileStamp, String)>);

impl StatusDigestCache {
    fn verified_with(
        &mut self,
        path: &Path,
        expected_bytes: u64,
        expected_sha: &str,
        hash: impl FnOnce(&Path) -> Result<String, String>,
    ) -> bool {
        let Ok(path) = std::fs::canonicalize(path) else { return false };
        let Ok(metadata) = std::fs::metadata(&path) else { return false };
        if metadata.len() != expected_bytes {
            return false;
        }
        let Ok(modified) = metadata.modified() else { return false };
        let stamp = StatusFileStamp { path, bytes: metadata.len(), modified };
        if self.0.iter().any(|(cached, sha)| *cached == stamp && sha == expected_sha) {
            return true;
        }
        if !hash(&stamp.path).is_ok_and(|sha| sha == expected_sha) {
            return false;
        }
        let Ok(after) = std::fs::metadata(&stamp.path) else { return false };
        if after.len() != stamp.bytes || after.modified().ok() != Some(stamp.modified) {
            return false;
        }
        self.0.retain(|(cached, _)| cached.path != stamp.path);
        if self.0.len() == 8 {
            self.0.remove(0);
        }
        self.0.push((stamp, expected_sha.to_owned()));
        true
    }

    fn verified(&mut self, path: &Path, expected_bytes: u64, expected_sha: &str) -> bool {
        self.verified_with(path, expected_bytes, expected_sha, digest)
    }
}

fn status_digest_cache() -> &'static Mutex<StatusDigestCache> {
    static CACHE: OnceLock<Mutex<StatusDigestCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(StatusDigestCache::default()))
}

pub(crate) fn qualified_webgpu_write_environment(app_data: &Path) -> bool {
    cfg!(target_os = "macos")
        && sam_ts::measured_m5_host()
        && cleaner_core::runtime::find(Some(app_data))
            .is_ok_and(|path| digest(&path).is_ok_and(|sha| sha == SAM_WEBGPU_RUNTIME_SHA256))
        && sam_ts::built_in_webgpu_available()
}

pub(crate) fn verify_graphs(dir: &Path) -> Result<(), String> {
    for graph in GRAPHS {
        let path = dir.join(graph.name);
        let size = std::fs::metadata(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .len();
        if size != graph.bytes {
            return Err(format!(
                "{} has {size} bytes; expected {}",
                path.display(),
                graph.bytes
            ));
        }
        let actual = digest(&path)?;
        if actual != graph.sha256 {
            return Err(format!("{} SHA-256 mismatch: {actual}", path.display()));
        }
    }
    Ok(())
}

pub(crate) fn graph_dir(app_data: &Path) -> Option<PathBuf> {
    // An explicit developer path is read-only and never removed by the UI.
    if let Ok(path) = std::env::var("MANGA_CLEANER_SAM_TS") {
        let path = PathBuf::from(path);
        if GRAPHS.iter().all(|graph| path.join(graph.name).is_file()) {
            return Some(path);
        }
    }
    let path = app_data.join("models/sam-ts-l");
    GRAPHS
        .iter()
        .all(|graph| path.join(graph.name).is_file())
        .then_some(path)
}

fn rt_path_with(
    app_data: &Path,
    mut verified: impl FnMut(&Path, u64, &str) -> bool,
) -> Option<PathBuf> {
    crate::run::model_search_paths(Some(app_data))
        .into_iter()
        .map(|dir| dir.join(crate::run::BALLOONS))
        .find(|path| verified(path, RT_BYTES, RT_SHA256))
}

fn rt_path(app_data: &Path) -> Option<PathBuf> {
    rt_path_with(app_data, |path, bytes, sha| {
        std::fs::metadata(path).is_ok_and(|meta| meta.len() == bytes)
            && digest(path).is_ok_and(|actual| actual == sha)
    })
}

fn full_rt_path_with(
    app_data: &Path,
    mut verified: impl FnMut(&Path, u64, &str) -> bool,
) -> Option<PathBuf> {
    let external = std::env::var("MANGA_CLEANER_RT_FULL")
        .ok()
        .map(PathBuf::from);
    let managed = app_data.join("models/rtdetr-v2-full").join(FULL_RT_NAME);
    external
        .into_iter()
        .chain(std::iter::once(managed))
        .chain(crate::run::model_search_paths(Some(app_data)).into_iter().map(|dir| dir.join(FULL_RT_NAME)))
        .find(|path| verified(path, FULL_RT_BYTES, FULL_RT_SHA256))
}

pub(crate) fn full_rt_path(app_data: &Path) -> Option<PathBuf> {
    full_rt_path_with(app_data, |path, bytes, sha| {
        std::fs::metadata(path).is_ok_and(|meta| meta.len() == bytes)
            && digest(path).is_ok_and(|actual| actual == sha)
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCapabilities {
    runtime_installed: bool,
    ctd_installed: bool,
    rt_installed: bool,
    full_rt_installed: bool,
    full_rt_managed: bool,
    full_rt_revision: &'static str,
    full_rt_file: ManifestFile,
    sam_installed: bool,
    sam_memory_ready: bool,
    sam_managed: bool,
    sam_revision: &'static str,
    sam_files: Vec<ManifestFile>,
    coo_status: &'static str,
    rt_backends: Vec<Backend>,
    sam_backends: Vec<Backend>,
    sam_write_qualified: bool,
    sam_write_note: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    name: &'static str,
    bytes: u64,
    sha256: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Backend {
    id: &'static str,
    platform: &'static str,
    qualified: bool,
    available: bool,
    selectable: bool,
    note: &'static str,
}

fn sam_backends(
    runtime_installed: bool,
    memory_ready: bool,
    webgpu_ready: bool,
    webgpu_qualified: bool,
) -> Vec<Backend> {
    let mut rows = vec![Backend {
        id: "ort-cpu",
        platform: "all",
        qualified: false,
        available: runtime_installed && memory_ready,
        selectable: runtime_installed && memory_ready,
        note: "Native ONNX CPU analysis. Saved parity evidence is from macOS; other platforms remain review-only.",
    }];
    if cfg!(target_os = "macos") {
        rows.extend([
            Backend { id: "ort-webgpu", platform: "macOS", qualified: webgpu_qualified,
                available: runtime_installed && memory_ready && webgpu_ready,
                selectable: runtime_installed && memory_ready && webgpu_ready,
                note: "45/45 exact saved Pillow-input masks on the measured Mac17,3 M5 with pinned ORT 1.28.0. Explicit WebGPU PNG component writes require that host/runtime, full graph assignment, and approval. Other Macs are review-only; CPU remains the default." },
            Backend { id: "ort-coreml", platform: "macOS", qualified: false, available: false, selectable: false, note: "Head was tested in a spike; full native path is unqualified." },
            Backend { id: "torch-mps", platform: "macOS", qualified: false, available: false, selectable: false, note: "PyTorch spike measured on Apple M5; no shipped PyTorch runtime." },
            Backend { id: "torch-cpu", platform: "macOS", qualified: false, available: false, selectable: false, note: "PyTorch reference only; no shipped PyTorch runtime." },
        ]);
    } else if cfg!(target_os = "windows") {
        rows.extend([
            Backend {
                id: "torch-cpu", platform: "Windows", qualified: false, available: false, selectable: false,
                note: "PyTorch reference path only; no shipped runtime or matching-hardware validation.",
            },
            Backend {
                id: "ort-directml",
                platform: "Windows",
                qualified: false,
                available: false,
                selectable: false,
                note: "Provider and SAM-TS-L graph require a Windows GPU qualification run.",
            },
            Backend {
                id: "ort-cuda",
                platform: "Windows",
                qualified: false,
                available: false,
                selectable: false,
                note: "Provider and SAM-TS-L graph require matching NVIDIA hardware validation.",
            },
            Backend {
                id: "ort-webgpu",
                platform: "Windows",
                qualified: false,
                available: false,
                selectable: false,
                note: "Provider and SAM-TS-L graph require a Windows GPU qualification run.",
            },
            Backend {
                id: "torch-cuda",
                platform: "Windows",
                qualified: false,
                available: false,
                selectable: false,
                note: "No shipped PyTorch runtime or matching-hardware validation.",
            },
        ]);
    } else if cfg!(target_os = "linux") {
        rows.extend([
            Backend {
                id: "torch-cpu", platform: "Linux", qualified: false, available: false, selectable: false,
                note: "PyTorch reference path only; no shipped runtime or matching-hardware validation.",
            },
            Backend {
                id: "ort-cuda",
                platform: "Linux",
                qualified: false,
                available: false,
                selectable: false,
                note: "Provider and SAM-TS-L graph require matching NVIDIA hardware validation.",
            },
            Backend {
                id: "ort-webgpu",
                platform: "Linux",
                qualified: false,
                available: false,
                selectable: false,
                note: "Provider and SAM-TS-L graph require matching GPU validation.",
            },
            Backend {
                id: "ort-migraphx",
                platform: "Linux",
                qualified: false,
                available: false,
                selectable: false,
                note: "No packaged or qualified MIGraphX path.",
            },
            Backend {
                id: "torch-cuda",
                platform: "Linux",
                qualified: false,
                available: false,
                selectable: false,
                note: "No shipped PyTorch runtime or matching-hardware validation.",
            },
        ]);
    }
    rows
}

fn rt_backends(runtime_installed: bool) -> Vec<Backend> {
    let directml = runtime_installed && cleaner_core::accel::Accelerator::DirectMl.is_available();
    let cuda = runtime_installed && cleaner_core::accel::Accelerator::Cuda.is_available();
    let mut rows = vec![Backend {
        id: "ort-cpu",
        platform: "all",
        qualified: cfg!(target_os = "macos") && sam_ts::measured_m5_host(),
        available: runtime_installed,
        selectable: runtime_installed,
        note: "Native CPU paths for the Full profile in tiles and the Small profile on the whole page. Saved parity evidence is from macOS.",
    }];
    if cfg!(target_os = "macos") {
        rows.extend([
            Backend { id: "ort-webgpu", platform: "macOS", qualified: false, available: false, selectable: false,
                note: "Both Ogkalu detector graphs retain CPU nodes and fail strict WebGPU placement." },
            Backend { id: "ort-coreml", platform: "macOS", qualified: false, available: false, selectable: false,
                note: "The Small profile failed CoreML session creation; the Full profile is unqualified in the desktop path." },
        ]);
    } else if cfg!(target_os = "windows") {
        rows.extend([
            Backend {
                id: "ort-directml",
                platform: "Windows",
                qualified: false,
                available: directml,
                selectable: directml,
                note: "Strict DirectML session build; model assignment is not yet verified on Windows hardware.",
            },
            Backend {
                id: "ort-cuda",
                platform: "Windows",
                qualified: false,
                available: cuda,
                selectable: cuda,
                note: "Strict CUDA session build; model assignment is not yet verified on Windows hardware.",
            },
        ]);
    } else if cfg!(target_os = "linux") {
        rows.extend([
            Backend {
                id: "ort-cuda",
                platform: "Linux",
                qualified: false,
                available: cuda,
                selectable: cuda,
                note: "Strict CUDA session build; model assignment is not yet verified on Linux hardware.",
            },
            Backend {
                id: "ort-webgpu",
                platform: "Linux",
                qualified: false,
                available: false,
                selectable: false,
                note: "Ogkalu detector graphs retained CPU nodes under strict WebGPU placement on macOS; Linux is unverified.",
            },
        ]);
    }
    rows
}

#[tauri::command]
pub async fn list_workflow_capabilities(app: tauri::AppHandle) -> Result<WorkflowCapabilities, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || workflow_capabilities(&app_data))
        .await
        .map_err(|e| e.to_string())
}

fn workflow_capabilities(app_data: &Path) -> WorkflowCapabilities {
    workflow_capabilities_with_room(app_data, cleaner_core::memory::room())
}

fn workflow_capabilities_with_room(app_data: &Path, room: Option<u64>) -> WorkflowCapabilities {
    let mut cache = status_digest_cache().lock().unwrap_or_else(|e| e.into_inner());
    let sam = graph_dir(app_data);
    let sam_installed = sam.as_ref().is_some_and(|dir| {
        GRAPHS.iter().all(|graph| {
            cache.verified(&dir.join(graph.name), graph.bytes, graph.sha256)
        })
    });
    let runtime = cleaner_core::runtime::find(Some(app_data)).ok();
    let runtime_installed = runtime.is_some();
    let runtime_ready = runtime.as_ref().is_some_and(|path| cleaner_core::runtime::load(path).is_ok());
    let webgpu_ready = cfg!(target_os = "macos")
        && runtime_ready
        && sam_ts::built_in_webgpu_available();
    // Parked CTD, LaMa and sidecar sessions count as room: analysis evicts
    // them before SAM and checks the room again, refusing by name if it is
    // still short. The status itself never evicts.
    let sam_memory_ready = room.is_some_and(|bytes| bytes >= SAM_MIN_ROOM_BYTES)
        || cleaner_core::residency::reclaimable_for_sam();
    let webgpu_qualified = webgpu_ready
        && sam_ts::measured_m5_host()
        && runtime.as_ref().is_some_and(|path| {
            cache.verified(
                path,
                std::fs::metadata(path).map_or(0, |meta| meta.len()),
                SAM_WEBGPU_RUNTIME_SHA256)
        });
    WorkflowCapabilities {
        runtime_installed,
        ctd_installed: crate::run::model_search_paths(Some(app_data)).into_iter()
            .any(|dir| dir.join(crate::run::DETECTOR).is_file()),
        rt_installed: rt_path_with(app_data, |path, bytes, sha| {
            cache.verified(path, bytes, sha)
        }).is_some(),
        sam_installed,
        full_rt_installed: full_rt_path_with(app_data, |path, bytes, sha| {
            cache.verified(path, bytes, sha)
        }).is_some(),
        full_rt_managed: app_data
            .join("models/rtdetr-v2-full")
            .join(FULL_RT_NAME)
            .is_file(),
        full_rt_revision: FULL_RT_REVISION,
        full_rt_file: ManifestFile {
            name: FULL_RT_NAME,
            bytes: FULL_RT_BYTES,
            sha256: FULL_RT_SHA256,
        },
        sam_memory_ready,
        sam_managed: sam
            .as_ref()
            .is_some_and(|dir| *dir == app_data.join("models/sam-ts-l")),
        sam_revision: SAM_REVISION,
        sam_files: GRAPHS
            .iter()
            .map(|graph| ManifestFile {
                name: graph.name,
                bytes: graph.bytes,
                sha256: graph.sha256,
            })
            .collect(),
        coo_status: "Rights unresolved. No bundled model, download, or desktop execution.",
        rt_backends: rt_backends(runtime_ready),
        sam_backends: sam_backends(
            runtime_ready,
            sam_memory_ready,
            webgpu_ready,
            webgpu_qualified,
        ),
        sam_write_qualified: webgpu_qualified && sam_memory_ready && sam_installed,
        sam_write_note: "Component writes require a PNG source analyzed with fully assigned WebGPU on the measured Mac17,3 M5, the pinned ORT 1.28.0 runtime, and explicit approval. CPU, JPEG, longstrip, and other hardware remain review-only.",
    }
}

#[tauri::command]
pub async fn import_full_rt(app: tauri::AppHandle, source_path: String) -> Result<bool, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = SAM_FILES.lock().map_err(|e| e.to_string())?;
        let source = PathBuf::from(source_path);
        if std::fs::metadata(&source).map_err(|e| e.to_string())?.len() != FULL_RT_BYTES
            || digest(&source)? != FULL_RT_SHA256
        {
            return Err(
                "The Ogkalu comic text & bubble detector (Full) graph size or SHA-256 does not match the pinned manifest".into(),
            );
        }
        let dir = app_data.join("models/rtdetr-v2-full");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let target = dir.join(FULL_RT_NAME);
        if source == target {
            return Ok(true);
        }
        let staged = dir.join(format!(".detector-{}.onnx", std::process::id()));
        std::fs::copy(&source, &staged).map_err(|e| e.to_string())?;
        if std::fs::metadata(&staged).map_err(|e| e.to_string())?.len() != FULL_RT_BYTES
            || digest(&staged)? != FULL_RT_SHA256
        {
            let _ = std::fs::remove_file(staged);
            return Err("The copied Ogkalu comic text & bubble detector (Full) graph failed verification".into());
        }
        let backup = dir.join(format!(".detector-{}.previous", std::process::id()));
        let old = target.is_file();
        if old {
            std::fs::rename(&target, &backup).map_err(|e| e.to_string())?;
        }
        if let Err(error) = std::fs::rename(&staged, &target) {
            if old {
                let _ = std::fs::rename(&backup, &target);
            }
            return Err(error.to_string());
        }
        if old {
            let _ = std::fs::remove_file(backup);
        }
        Ok(true)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn remove_full_rt(app: tauri::AppHandle) -> Result<bool, String> {
    let _guard = SAM_FILES.lock().map_err(|e| e.to_string())?;
    let target = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("models/rtdetr-v2-full")
        .join(FULL_RT_NAME);
    match std::fs::remove_file(target) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

#[tauri::command]
pub async fn verify_sam_ts(app: tauri::AppHandle) -> Result<bool, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let dir = graph_dir(&app_data).ok_or("The SAM-TS-L lettering mask graphs are not installed")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = SAM_FILES.lock().map_err(|e| e.to_string())?;
        verify_graphs(&dir).map(|_| true)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn sam_bootstrap_source(app: &tauri::AppHandle, app_data: &Path) -> Result<PathBuf, String> {
    let bundled = app.path().resource_dir().map_err(|e| e.to_string())?.join("sam-bootstrap");
    if bundled.join("bootstrap.py").is_file() && bundled.join("export_mask.py").is_file() {
        return Ok(bundled);
    }
    if cfg!(debug_assertions) {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        if checkout.join("sam-bootstrap/bootstrap.py").is_file() {
            let target = app_data.join("models/.sam-ts-l-bootstrap/source");
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            for (from, to) in [
                (checkout.join("sam-bootstrap/bootstrap.py"), "bootstrap.py"),
                (checkout.join("spikes/sam-ts-l/export_mask.py"), "export_mask.py"),
                (checkout.join("spikes/sam-ts-l/environment.txt"), "environment.txt"),
                (checkout.join("spikes/sam-ts-l/fixtures/synthetic_page.png"), "synthetic_page.png"),
            ] {
                std::fs::copy(from, target.join(to)).map_err(|e| e.to_string())?;
            }
            return Ok(target);
        }
    }
    Err("This app build does not include the SAM-TS-L lettering mask exporter".into())
}

fn sam_uv() -> PathBuf {
    let name = if cfg!(windows) { "manga-cleaner-uv.exe" } else { "manga-cleaner-uv" };
    std::env::current_exe().ok()
        .and_then(|path| path.parent().map(|dir| dir.join(name)))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("uv"))
}

fn sam_step(app: &tauri::AppHandle, name: &'static str, mut command: Command, root: &Path) -> Result<(), String> {
    let _ = app.emit("sam-install://progress", serde_json::json!({"step": name, "state": "start"}));
    let log = root.join(format!("{name}.log"));
    let output = std::fs::File::create(&log).map_err(|e| e.to_string())?;
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::null())
        .stderr(Stdio::from(output)).spawn()
        .map_err(|e| format!("The SAM-TS-L lettering mask {name} could not start: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(2 * 60 * 60);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if status.success() {
                let _ = app.emit("sam-install://progress", serde_json::json!({"step": name, "state": "done"}));
                return Ok(());
            }
            let detail = std::fs::read_to_string(&log).unwrap_or_default();
            let end = detail.chars().rev().take(600).collect::<String>().chars().rev().collect::<String>();
            return Err(format!("The SAM-TS-L lettering mask {name} failed: {end}"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("The SAM-TS-L lettering mask {name} timed out; retry to continue"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[tauri::command]
pub async fn install_sam_ts(app: tauri::AppHandle) -> Result<bool, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    if graph_dir(&app_data).is_some_and(|dir| verify_graphs(&dir).is_ok()) { return Ok(true); }
    let source = sam_bootstrap_source(&app, &app_data)?;
    let staged = tauri::async_runtime::spawn_blocking({
        let app = app.clone();
        move || -> Result<PathBuf, String> {
            let _install = SAM_INSTALL.lock().map_err(|e| e.to_string())?;
            let root = app_data.join("models/.sam-ts-l-bootstrap");
            std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
            let (managed, prefix) = crate::flux_install::managed_python(&app, &root, "3.12")?;
            let uv = sam_uv();
            let venv = root.join("venv");
            let mut setup = Command::new(&uv);
            setup.args(["venv", "--python", &managed]).arg(&venv).env("UV_NO_CONFIG", "1");
            sam_step(&app, "environment", setup, &root)?;
            let python = if cfg!(windows) { venv.join("Scripts/python.exe") } else { venv.join("bin/python") };
            let mut dependencies = Command::new(&uv);
            dependencies.args(["pip", "sync", "--python"]).arg(&python).arg(source.join("environment.txt"))
                .env("UV_NO_CONFIG", "1");
            sam_step(&app, "dependencies", dependencies, &root)?;
            let mut bootstrap = Command::new(&python);
            bootstrap.args(prefix).arg(source.join("bootstrap.py"))
                .arg("--root").arg(&root).arg("--source").arg(&source);
            sam_step(&app, "sources-export", bootstrap, &root)?;
            let proof = root.join("proof");
            verify_graphs(&proof)?;
            let _ = app.emit("sam-install://progress", serde_json::json!({"step": "verify", "state": "done"}));
            Ok(proof)
        }
    }).await.map_err(|e| e.to_string())??;
    import_sam_ts(app, staged.to_string_lossy().into_owned()).await
}

#[tauri::command]
pub async fn import_sam_ts(app: tauri::AppHandle, source_dir: String) -> Result<bool, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = SAM_FILES.lock().map_err(|e| e.to_string())?;
        let source = PathBuf::from(source_dir);
        verify_graphs(&source)?;
        let target = app_data.join("models/sam-ts-l");
        if source == target {
            return Ok(true);
        }
        let models = app_data.join("models");
        std::fs::create_dir_all(&models).map_err(|e| e.to_string())?;
        sweep_removed_sam_dirs(&models);
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        );
        let staged = models.join(format!(".sam-ts-l-import-{nonce}"));
        let backup = models.join(format!(".sam-ts-l-previous-{nonce}"));
        std::fs::create_dir(&staged).map_err(|e| e.to_string())?;
        for graph in GRAPHS {
            if let Err(error) = std::fs::copy(source.join(graph.name), staged.join(graph.name)) {
                let _ = std::fs::remove_dir_all(&staged);
                return Err(error.to_string());
            }
        }
        if let Err(error) = verify_graphs(&staged) {
            let _ = std::fs::remove_dir_all(&staged);
            return Err(error);
        }
        let old = target.exists();
        if old {
            std::fs::rename(&target, &backup).map_err(|e| e.to_string())?;
        }
        if let Err(error) = std::fs::rename(&staged, &target) {
            if old {
                let _ = std::fs::rename(&backup, &target);
            }
            return Err(error.to_string());
        }
        if old {
            let _ = std::fs::remove_dir_all(backup);
        }
        Ok(true)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn remove_sam_ts(app: tauri::AppHandle) -> Result<bool, String> {
    let _guard = SAM_FILES.lock().map_err(|e| e.to_string())?;
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    remove_managed_sam_dir(&app_data.join("models"))
}

fn remove_managed_sam_dir(models: &Path) -> Result<bool, String> {
    sweep_removed_sam_dirs(models);
    let managed = models.join("sam-ts-l");
    if !managed.exists() {
        return Ok(false);
    }
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    let removed = models.join(format!(".sam-ts-l-deleted-{nonce}"));
    // A failed directory rename leaves both graphs available. Once renamed,
    // the complete group disappears from model lookup in one filesystem step.
    std::fs::rename(&managed, &removed).map_err(|e| e.to_string())?;
    // A mapped file can prevent cleanup on Windows. Its private tombstone is
    // retried on the next import/remove; it is never a discoverable model.
    let _ = std::fs::remove_dir_all(&removed);
    Ok(true)
}

fn sweep_removed_sam_dirs(models: &Path) {
    let Ok(entries) = std::fs::read_dir(models) else { return };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(".sam-ts-l-deleted-"))
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    analysis_id: Option<String>,
    source_sha256: String,
    rt_model_sha256: Option<&'static str>,
    sam_encoder_sha256: Option<&'static str>,
    sam_head_sha256: Option<&'static str>,
    mask_sha256: Option<String>,
    workflow: String,
    rt_profile: Option<String>,
    rt_backend: Option<&'static str>,
    sam_backend: Option<&'static str>,
    remote_source: Option<String>,
    sam_write_eligible: bool,
    sam_webgpu_nodes: Option<(u64, u64)>,
    sam_cpu_fallback_nodes: Option<(u64, u64)>,
    sam_process_high_water_bytes: Option<u64>,
    sam_process_phys_footprint_bytes: Option<u64>,
    sam_sampled_metal_high_water_bytes: Option<u64>,
    evidence: ReviewEvidence,
    source_data_url: String,
    mask_data_url: Option<String>,
    timings_ms: Timings,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Timings {
    rt_load: u128,
    rt_page: u128,
    sam_load: u128,
    sam_prepare: u128,
    sam_encoder: u128,
    sam_head: u128,
    sam_restore: u128,
}

fn encode_mask(width: u32, height: u32, bits: &[u8]) -> Result<String, String> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .map_err(|e| e.to_string())?
            .write_image_data(bits)
            .map_err(|e| e.to_string())?;
    }
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn display_source(bytes: &[u8]) -> Result<Raster,String> {
    let native=cleaner_core::image::decode(bytes).or_else(|_| cleaner_core::image::foreign::decode(bytes)).map_err(|e|e.to_string())?;
    Ok(cleaner_core::image::orientation::from_bytes(bytes).raster(&native))
}

fn source_preview(bytes: &[u8]) -> Result<String,String> {
    let shown=cleaner_core::image::proxy::display(&display_source(bytes)?).map_err(|e|e.to_string())?;
    let png=cleaner_core::image::encode(&shown,cleaner_core::image::Format::Png).map_err(|e|e.to_string())?;
    Ok(format!("data:image/png;base64,{}",base64::engine::general_purpose::STANDARD.encode(png)))
}

fn encode_analysis_previews(
    cancel: &sam_ts::Cancellation,
    width: u32,
    height: u32,
    mask: Option<&[u8]>,
    source: &[u8],
) -> Result<(Option<String>, String), String> {
    cancel.check()?;
    let mask_data_url = mask.map(|bits| encode_mask(width, height, bits)).transpose()?;
    let source_data_url = source_preview(source)?;
    Ok((mask_data_url, source_data_url))
}

fn component_support(analysis: &StoredAnalysis, component_id: &str) -> Result<Mask, String> {
    let wanted: usize = component_id
        .strip_prefix("sam-")
        .ok_or("Only a SAM component can grant write support")?
        .parse()
        .map_err(|_| "Invalid SAM component id")?;
    if wanted == 0 || analysis.mask.len() != analysis.width as usize * analysis.height as usize {
        return Err("Invalid SAM component or mask dimensions".into());
    }
    let mut visited = vec![false; analysis.mask.len()];
    let mut number = 0usize;
    for start in 0..analysis.mask.len() {
        if visited[start] || analysis.mask[start] == 0 {
            continue;
        }
        number += 1;
        let mut queue = std::collections::VecDeque::from([start]);
        let mut pixels = Vec::new();
        visited[start] = true;
        let (mut left, mut top) = (analysis.width, analysis.height);
        let (mut right, mut bottom) = (0u32, 0u32);
        while let Some(at) = queue.pop_front() {
            let x = (at % analysis.width as usize) as u32;
            let y = (at / analysis.width as usize) as u32;
            if number == wanted {
                pixels.push((x, y));
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
            for ny in y.saturating_sub(1)..=(y + 1).min(analysis.height - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(analysis.width - 1) {
                    let next = ny as usize * analysis.width as usize + nx as usize;
                    if analysis.mask[next] != 0 && !visited[next] {
                        visited[next] = true;
                        queue.push_back(next);
                    }
                }
            }
        }
        if number == wanted {
            let mut support = Mask::empty(Rect::new(
                left as i64,
                top as i64,
                right - left,
                bottom - top,
            ));
            for (x, y) in pixels {
                support.set(x as i64, y as i64, true);
            }
            return Ok(support);
        }
    }
    Err("SAM component is absent from this analysis".into())
}

fn empty_correction() -> MaskRaster {
    MaskRaster {
        bounds: Rect::new(0, 0, 0, 0),
        bits: Vec::new(),
    }
}

fn source_for_page(job: &Job, page_index: usize) -> Result<(usize, String, Raster), String> {
    let source_idx =
        Library::resolve_page(&job.project, page_index).ok_or("Page is no longer in chapter")?;
    let path = job
        .source_path(source_idx)
        .ok_or("Chapter source is missing")?;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    // The saved SAM parity evidence excludes JPEG decoder differences. Keep
    // review available for JPEG, but do not turn its mask into write authority.
    if !bytes.starts_with(PNG_MAGIC) {
        return Err(
            "Component writes currently require a PNG chapter source; JPEG parity is unresolved"
                .into(),
        );
    }
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let page = job.display_page(source_idx,&bytes).map_err(|e|e.to_string())?;
    Ok((source_idx, sha, page))
}

fn validate_write_format(page: &Raster) -> Result<(), String> {
    if page.mode == cleaner_core::image::ColorMode::Indexed {
        return Err("Component writing cannot fill indexed palette PNG sources".into());
    }
    if page.depth.bits() < 8 {
        return Err("Component writing cannot fill sub-8-bit PNG sources".into());
    }
    Ok(())
}

fn ensure_unedited_support(
    job: &Job,
    source_idx: usize,
    support: &Mask,
    replacing_region: &str,
) -> Result<(), String> {
    if job.project.strip.mode != StripMode::Single {
        return Err("Component writing currently requires a paginated chapter".into());
    }
    for record in &job.project.patches {
        if !record.visible || record.source_idx != source_idx || record.id == replacing_region {
            continue;
        }
        let bbox=cleaner_core::project::orientation::display_bbox(&job.project,record);
        if bbox.right() <= support.bounds.x
            || bbox.x >= support.bounds.right()
            || bbox.bottom() <= support.bounds.y
            || bbox.y >= support.bounds.bottom()
        {
            continue;
        }
        let existing = job.load_display_patch(record).map_err(|e| e.to_string())?;
        for y in support.bounds.y..support.bounds.bottom() {
            for x in support.bounds.x..support.bounds.right() {
                if support.contains(x, y) && existing.mask.contains(x, y) {
                    return Err("Analyzed component overlaps an existing visible edit; analyze a fresh page or use the editor mask tools".into());
                }
            }
        }
    }
    Ok(())
}

fn next_component_order(job: &Job) -> Result<u32, String> {
    match job.project.patches.iter().map(|p| p.order).max() {
        Some(last) => last.checked_add(1).ok_or("Patch order is exhausted".into()),
        None => Ok(0),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedWrite {
    plan_id: String,
    component_id: String,
    bounds: Rect,
    support_pixels: usize,
    support_data_url: String,
    support_sha256: String,
    support_version: &'static str,
    render_version: &'static str,
    source_sha256: String,
    underlay_sha256: String,
    model_sha256: String,
    plan_identity_sha256: String,
    padding_px: u32,
    correction_revision: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedComponentCorrection {
    region_id: String,
    additions: MaskRaster,
    removals: MaskRaster,
    padding_px: u32,
    correction_revision: u64,
    plan_revision: u64,
}

fn component_region_id(
    chapter_id: &str,
    page_index: usize,
    component_id: &str,
    source_sha256: &str,
    model_sha256: &str,
) -> String {
    format!(
        "{}-hreview-{}-{}-{}",
        crate::library::page_id(chapter_id, page_index),
        component_id,
        &source_sha256[..12],
        &model_sha256[..12]
    )
}

fn load_component_correction_at(
    library: &Library,
    analysis_id: &str,
    chapter_id: &str,
    page_index: usize,
    component_id: &str,
) -> Result<Option<SavedComponentCorrection>, String> {
    let store = review_store().lock().map_err(|e| e.to_string())?;
    let analysis = store
        .analysis
        .as_ref()
        .ok_or("Analysis expired; analyze the page again")?;
    if analysis.id != analysis_id {
        return Err("Analysis expired; analyze the page again".into());
    }
    if analysis.sam_backend == "remote" {
        return Err("Remote analysis is review-only and cannot load a component correction".into());
    }
    let base = component_support(analysis, component_id)?;
    let path = library.resolve_chapter(chapter_id).map_err(|e| e.to_string())?;
    // The review store is let go for the chapter's lock and taken back after
    // it: the chapter first is the order a remote analysis takes the two in
    // (`confirm_and_run`), and a wait on another process's run must not hold
    // the store every other review command needs. Same id, same analysis.
    drop(store);
    let _job_lock = crate::run::lock_job(&path)?;
    let store = review_store().lock().map_err(|e| e.to_string())?;
    let analysis = store.analysis.as_ref().filter(|analysis| analysis.id == analysis_id)
        .ok_or("Analysis expired; analyze the page again")?;
    let job = Job::open(&path).map_err(|e| e.to_string())?;
    let (_, source_sha256, page) = source_for_page(&job, page_index)?;
    if source_sha256 != analysis.source_sha256
        || page.width != analysis.width
        || page.height != analysis.height
    {
        return Err("The selected chapter page does not match the analyzed source".into());
    }
    let region_id = component_region_id(
        chapter_id,
        page_index,
        component_id,
        &source_sha256,
        &analysis.model_sha256,
    );
    let Some((plan, _)) = job.load_text_shape_plan(&region_id).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    if plan.source_sha256 != source_sha256
        || plan.model_id.as_deref() != Some(analysis.model_sha256.as_str())
        || plan.base_mask != MaskRaster::from(base)
    {
        return Err("Saved correction does not match this analysis; analyze the page again".into());
    }
    Ok(Some(SavedComponentCorrection {
        region_id,
        additions: plan.additions,
        removals: plan.removals,
        padding_px: plan.padding_px,
        correction_revision: plan.correction_revision,
        plan_revision: plan.plan_revision,
    }))
}

#[tauri::command]
pub async fn load_component_correction(
    app: tauri::AppHandle,
    analysis_id: String,
    chapter_id: String,
    page_index: usize,
    component_id: String,
) -> Result<Option<SavedComponentCorrection>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        load_component_correction_at(
            &Library::for_app(&app).map_err(|e| e.to_string())?,
            &analysis_id,
            &chapter_id,
            page_index,
            &component_id,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[allow(clippy::too_many_arguments)]
fn prepare_component_at(
    library: &Library,
    write_environment_qualified: bool,
    analysis_id: &str,
    chapter_id: &str,
    page_index: usize,
    component_id: &str,
    allow_outside_bubbles: bool,
    padding_px: u32,
    additions: MaskRaster,
    removals: MaskRaster,
    correction_revision: u64,
) -> Result<PreparedWrite, String> {
    let store = review_store().lock().map_err(|e| e.to_string())?;
    let analysis = store
        .analysis
        .as_ref()
        .ok_or("Analysis expired; analyze the page again")?;
    if analysis.id != analysis_id {
        return Err("Analysis expired; analyze the page again".into());
    }
    if analysis.sam_backend == "remote" {
        return Err("Remote analysis is review-only and cannot prepare a component write".into());
    }
    if !write_environment_qualified
        || !analysis.write_eligible
        || analysis.sam_backend != "ort-webgpu"
        || analysis.runtime_sha256 != Some(SAM_WEBGPU_RUNTIME_SHA256)
    {
        return Err(
            "This analysis is not qualified for component writing on this host and runtime".into(),
        );
    }
    if !allow_outside_bubbles
        && !analysis.bubble_associated_components.contains(component_id)
    {
        return Err("Outside-bubble component is held until explicitly permitted".into());
    }
    let base = component_support(analysis, component_id)?;
    if base.is_empty() {
        return Err("Empty component has no write support".into());
    }
    let path = library
        .resolve_chapter(chapter_id)
        .map_err(|e| e.to_string())?;
    // Let go for the chapter's lock and taken back, as in
    // `load_component_correction_at`.
    drop(store);
    let _job_lock = crate::run::lock_job(&path)?;
    let mut store = review_store().lock().map_err(|e| e.to_string())?;
    let analysis = store.analysis.as_ref().filter(|analysis| analysis.id == analysis_id)
        .ok_or("Analysis expired; analyze the page again")?;
    let job = Job::open(&path).map_err(|e| e.to_string())?;
    let (source_idx, source_sha256, page) = source_for_page(&job, page_index)?;
    validate_write_format(&page)?;
    if source_sha256 != analysis.source_sha256
        || page.width != analysis.width
        || page.height != analysis.height
    {
        return Err("The selected chapter page does not match the analyzed source".into());
    }
    let region_id = component_region_id(
        chapter_id,
        page_index,
        component_id,
        &source_sha256,
        &analysis.model_sha256,
    );
    let existing = job
        .project
        .patches
        .iter()
        .find(|record| record.id == region_id);
    if existing.is_some_and(|record| {
        record.geometry_policy != cleaner_core::text_shape::GeometryPolicy::TextShape
    }) {
        return Err("Existing region does not use text-shaped geometry".into());
    }
    let order = match existing {
        Some(record) => record.order,
        None => next_component_order(&job)?,
    };
    let old_plan = job
        .load_text_shape_plan(&region_id)
        .map_err(|e| e.to_string())?;
    if let Some((old, _)) = &old_plan {
        if old.base_mask != MaskRaster::from(base.clone()) || old.base_revision != 1 {
            return Err("Saved SAM base mask changed; analyze the page again".into());
        }
        if correction_revision <= old.correction_revision
            && (additions != old.additions || removals != old.removals)
        {
            return Err("Mask corrections changed without a new correction revision".into());
        }
    }
    let plan_revision = old_plan.as_ref().map_or(Ok(1), |(old, _)| {
        old.plan_revision
            .checked_add(1)
            .ok_or("Mask plan revision is exhausted")
    })?;
    let mut mask_plan = MaskPlan {
        version: MASK_PLAN_VERSION,
        region_id: region_id.clone(),
        source_sha256: source_sha256.clone(),
        lower_composite_sha256: "preparing".into(),
        algorithm_id: "sam-ts-l-high-res-lettering".into(),
        model_id: Some(analysis.model_sha256.clone()),
        candidate_bounds: analysis.candidate_bounds.get(component_id).copied().unwrap_or(base.bounds),
        refinement_crop: base.bounds,
        base_revision: 1,
        correction_revision,
        plan_revision,
        base_mask: base.into(),
        additions,
        removals,
        padding_px,
        model_hole_margin_px: 0,
        reading_context: Rect::new(0, 0, 1, 1),
        blend_alpha: None,
        quality: MaskQualityState::Ready,
    };
    let provisional = mask_plan
        .prepare(page.width, page.height)
        .map_err(|e| e.to_string())?;
    let support = provisional.write_support.to_mask();
    mask_plan.refinement_crop = support.bounds;
    ensure_unedited_support(&job, source_idx, &support, &region_id)?;
    let input = crate::underlay::read(&job, source_idx, &page, support.bounds, order, None)?;
    mask_plan.lower_composite_sha256 = input.digest.clone();
    mask_plan.reading_context = input.window;
    let prepared_mask_plan = mask_plan
        .prepare(page.width, page.height)
        .map_err(|e| e.to_string())?;
    let support_sha256 = prepared_mask_plan.identity.support_sha256.clone();
    let plan_id = format!(
        "{:x}",
        Sha256::digest(format!(
            "write-plan-v2:{}:{SAM_WEBGPU_RUNTIME_SHA256}:{analysis_id}:{chapter_id}:{page_index}:{component_id}:{source_sha256}:{support_sha256}:{}:{}:{}:{order}",
            analysis.sam_backend,
            input.digest,
            input.predecessors,
            prepared_mask_plan.identity.identity_sha256,
        ))
    );
    let prepared = PreparedComponent {
        id: plan_id.clone(),
        analysis_id: analysis_id.into(),
        sam_backend: analysis.sam_backend,
        runtime_sha256: SAM_WEBGPU_RUNTIME_SHA256,
        chapter_id: chapter_id.into(),
        page_index,
        component_id: component_id.into(),
        source_sha256: source_sha256.clone(),
        model_sha256: analysis.model_sha256.clone(),
        support_sha256: support_sha256.clone(),
        underlay_sha256: input.digest.clone(),
        predecessors_sha256: input.predecessors,
        order,
        support: support.clone(),
        mask_plan,
        prepared_mask_plan: prepared_mask_plan.clone(),
    };
    let model_sha256 = analysis.model_sha256.clone();
    store.prepared = Some(prepared);
    Ok(PreparedWrite {
        plan_id,
        component_id: component_id.into(),
        bounds: support.bounds,
        support_pixels: support.count(),
        support_data_url: encode_mask(support.bounds.w, support.bounds.h, &support.bits)?,
        support_sha256,
        support_version: WRITE_SUPPORT_VERSION,
        render_version: RENDER_VERSION,
        source_sha256,
        underlay_sha256: input.digest,
        model_sha256,
        plan_identity_sha256: prepared_mask_plan.identity.identity_sha256,
        padding_px,
        correction_revision,
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn prepare_component_write(
    app: tauri::AppHandle,
    analysis_id: String,
    chapter_id: String,
    page_index: usize,
    component_id: String,
    allow_outside_bubbles: Option<bool>,
    padding_px: Option<u32>,
    additions: Option<MaskRaster>,
    removals: Option<MaskRaster>,
    correction_revision: Option<u64>,
) -> Result<PreparedWrite, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
        prepare_component_at(
            &Library::for_app(&app).map_err(|e| e.to_string())?,
            qualified_webgpu_write_environment(&app_data),
            &analysis_id,
            &chapter_id,
            page_index,
            &component_id,
            allow_outside_bubbles.unwrap_or(false),
            padding_px.unwrap_or(0),
            additions.unwrap_or_else(empty_correction),
            removals.unwrap_or_else(empty_correction),
            correction_revision.unwrap_or(0),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn render_bounded_fill(input: &crate::underlay::Input, support: &Mask) -> Result<Raster, String> {
    cleaner_core::image::color::validate_editing(&input.image).map_err(|e|e.to_string())?;
    let bounds = support.bounds;
    if !Rect::new(
        input.window.x,
        input.window.y,
        input.window.w,
        input.window.h,
    )
    .contains(bounds.x, bounds.y)
        || bounds.right() > input.window.right()
        || bounds.bottom() > input.window.bottom()
    {
        return Err("Write support is outside the composited underlay".into());
    }
    let samples = input.image.mode.samples();
    let mut medians = Vec::with_capacity(samples);
    let ring = bounds.grown(8, input.window.right() as u32, input.window.bottom() as u32);
    for channel in 0..samples {
        let mut values = Vec::new();
        for y in ring.y.max(input.window.y)..ring.bottom().min(input.window.bottom()) {
            for x in ring.x.max(input.window.x)..ring.right().min(input.window.right()) {
                if bounds.contains(x, y) || support.contains(x, y)
                    || cleaner_core::image::color::alpha(&input.image,(x-input.window.x) as u32,(y-input.window.y) as u32) == 0.0 {
                    continue;
                }
                values.push(input.image.sample(
                    (x - input.window.x) as u32,
                    (y - input.window.y) as u32,
                    channel,
                ));
            }
        }
        if values.is_empty() {
            return Err("No surrounding pixels available for bounded fill".into());
        }
        values.sort_unstable();
        medians.push(values[values.len() / 2]);
    }
    let mut pixels = Raster {
        width: bounds.w,
        height: bounds.h,
        mode: input.image.mode,
        depth: input.image.depth,
        icc: input.image.icc.clone(),
        palette: input.image.palette.clone(),
        trns: input.image.trns.clone(),
        srgb_intent: input.image.srgb_intent, color: input.image.color.after_edit(),
        data: vec![
            0;
            {
                let bits = bounds.w as usize * samples * input.image.depth.bits() as usize;
                bits.div_ceil(8) * bounds.h as usize
            }
        ],
    };
    for y in 0..bounds.h {
        for x in 0..bounds.w {
            let px = bounds.x + x as i64;
            let py = bounds.y + y as i64;
            let inside = support.contains(px,py) && cleaner_core::image::color::alpha(&input.image,(px-input.window.x) as u32,(py-input.window.y) as u32)>0.0;
            for (channel, &median) in medians.iter().enumerate() {
                let original = input.image.sample(
                    (px - input.window.x) as u32,
                    (py - input.window.y) as u32,
                    channel,
                );
                let value = if inside
                    && input.image.mode.alpha_channel() != Some(channel)
                {
                    if input.image.mode == cleaner_core::image::ColorMode::Indexed {
                        let opacity = |i:u16| input.image.trns.as_ref().and_then(|t|t.get(i as usize)).copied().unwrap_or(255);
                        if opacity(original)==opacity(median) { median } else { original }
                    } else { median }
                } else {
                    original
                };
                pixels.set_sample(x, y, channel, value);
            }
            if inside { cleaner_core::image::color::avoid_transparent_key(&mut pixels,x,y); }
        }
    }
    Ok(pixels)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedWrite {
    region: crate::library::ApiRegion,
    page_status: String,
    region_id: String,
}

fn apply_component_at(
    library: &Library,
    write_environment_qualified: bool,
    plan_id: &str,
    approved_support_sha256: &str,
) -> Result<AppliedWrite, String> {
    let store = review_store().lock().map_err(|e| e.to_string())?;
    let plan = store
        .prepared
        .as_ref()
        .ok_or("Prepared write expired; preview the component again")?;
    if plan.id != plan_id || plan.support_sha256 != approved_support_sha256 {
        return Err("Approval does not match the prepared support raster".into());
    }
    if !write_environment_qualified
        || plan.sam_backend != "ort-webgpu"
        || plan.runtime_sha256 != SAM_WEBGPU_RUNTIME_SHA256
    {
        return Err("This prepared write is not qualified on this host and runtime".into());
    }
    if store.analysis.as_ref().is_none_or(|a| {
        a.id != plan.analysis_id
            || a.model_sha256 != plan.model_sha256
            || !a.write_eligible
            || a.sam_backend != plan.sam_backend
            || a.runtime_sha256 != Some(plan.runtime_sha256)
    }) {
        return Err("Analysis changed; preview the component again".into());
    }
    let path = library
        .resolve_chapter(&plan.chapter_id)
        .map_err(|e| e.to_string())?;
    // Let go for the chapter's lock and taken back, as in
    // `load_component_correction_at`. A new analysis clears the prepared
    // write, so the same plan id is the same plan over the same analysis.
    drop(store);
    let _job_lock = crate::run::lock_job(&path)?;
    let mut store = review_store().lock().map_err(|e| e.to_string())?;
    let plan = store.prepared.as_ref().filter(|plan| plan.id == plan_id)
        .ok_or("Prepared write expired; preview the component again")?;
    let mut job = Job::open(&path).map_err(|e| e.to_string())?;
    let (source_idx, source_sha256, page) = source_for_page(&job, plan.page_index)?;
    if source_sha256 != plan.source_sha256 {
        return Err("Source changed since approval".into());
    }
    let support = &plan.support;
    if plan
        .prepared_mask_plan
        .verify_against(&plan.mask_plan, page.width, page.height)
        .is_err()
        || plan.prepared_mask_plan.identity.support_sha256 != plan.support_sha256
        || plan.prepared_mask_plan.write_support.to_mask() != *support
        || support.is_empty()
    {
        return Err("Approved support raster changed".into());
    }
    ensure_unedited_support(&job, source_idx, support, &plan.mask_plan.region_id)?;
    let region_id = plan.mask_plan.region_id.clone();
    let existing = job
        .project
        .patches
        .iter()
        .find(|record| record.id == region_id);
    if existing.is_some_and(|record| {
        record.geometry_policy != cleaner_core::text_shape::GeometryPolicy::TextShape
    }) {
        return Err("Existing region does not use text-shaped geometry".into());
    }
    let order = match existing {
        Some(record) => record.order,
        None => next_component_order(&job)?,
    };
    if order != plan.order {
        return Err("Region order changed since preview".into());
    }
    let stored_revision = job
        .load_text_shape_plan(&region_id)
        .map_err(|e| e.to_string())?
        .map_or(0, |(old, _)| old.plan_revision);
    if Some(plan.mask_plan.plan_revision) != stored_revision.checked_add(1) {
        return Err("Mask plan revision changed since preview".into());
    }
    let input = crate::underlay::read(&job, source_idx, &page, support.bounds, order, None)?;
    if input.digest != plan.underlay_sha256 || input.predecessors != plan.predecessors_sha256 {
        return Err("Visible page changed since preview; prepare the component again".into());
    }
    let pixels = render_bounded_fill(&input, support)?;
    let patch = Patch {
        id: region_id.clone(),
        mask: support.clone(),
        ink: plan.prepared_mask_plan.corrected_base.to_mask(),
        pixels,
        order,
        visible: true,
        provenance: Provenance {
            engine: PatchEngine::Fill,
            engine_version: env!("CARGO_PKG_VERSION").into(),
            model_sha256: Some(plan.model_sha256.clone()),
            execution_provider: plan.sam_backend.into(),
            params_snapshot: serde_json::json!({
                "workflow": "approved-component", "analysis_id": plan.analysis_id,
                "component_id": plan.component_id, "source_sha256": plan.source_sha256,
                "support_version": WRITE_SUPPORT_VERSION, "support_sha256": plan.support_sha256,
                "padding_px": plan.mask_plan.padding_px,
                "base_revision": plan.mask_plan.base_revision,
                "correction_revision": plan.mask_plan.correction_revision,
                "plan_revision": plan.mask_plan.plan_revision,
                "plan_identity_sha256": plan.prepared_mask_plan.identity.identity_sha256,
                "render_version": RENDER_VERSION, "underlay_sha256": plan.underlay_sha256,
                "predecessors_sha256": plan.predecessors_sha256,
                "sam_checkpoint": cleaner_core::text_groups::SAM_TS_L_REPOSITORY,
                "sam_revision": SAM_REVISION, "sam_encoder_sha256": GRAPHS[0].sha256,
                "sam_head_sha256": GRAPHS[1].sha256, "fill_mode": "solid",
                "sam_backend": plan.sam_backend, "ort_runtime_sha256": SAM_WEBGPU_RUNTIME_SHA256,
            }),
            mask_sha256: crate::run::mask_digest(support),
            source_sha256,
            cloud: None,
            created: crate::run::now(),
        },
    };
    job.store_text_shape_plan(source_idx, &plan.mask_plan, &plan.prepared_mask_plan)
        .map_err(|e| e.to_string())?;
    job.complete_text_shape_region(
        source_idx,
        &patch,
        &plan.prepared_mask_plan.identity.identity_sha256,
        None,
    )
    .map_err(|e| e.to_string())?;
    crate::underlay::refresh_dependencies(&mut job, &region_id)?;
    let (region, page_status) =
        crate::library::region_and_status(&plan.chapter_id, &job.project, source_idx, &region_id)
            .ok_or("Saved component could not be found in chapter")?;
    store.prepared = None;
    Ok(AppliedWrite {
        region,
        page_status,
        region_id,
    })
}

#[tauri::command]
pub async fn apply_component_write(
    app: tauri::AppHandle,
    plan_id: String,
    approved_support_sha256: String,
) -> Result<AppliedWrite, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
        apply_component_at(
            &Library::for_app(&app).map_err(|e| e.to_string())?,
            qualified_webgpu_write_environment(&app_data),
            &plan_id,
            &approved_support_sha256,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn store_remote_analysis(
    provider: &str,
    capability: &str,
    model_identity: &str,
    source: &[u8],
    width: u32,
    height: u32,
    mask: Option<Vec<u8>>,
    boxes: Vec<cleaner_core::balloon::BalloonBox>,
) -> Result<Analysis, String> {
    if source.len() > 20_000_000 {
        return Err("Source page exceeds the 20 MB review-preview limit".into());
    }
    let source_sha256 = format!("{:x}", Sha256::digest(source));
    let page = display_source(source)?;
    // The production grouping over the cloud's answer: the same models,
    // tiles and seams a cloud run groups by, and the page's pixels for the
    // ink estimate under a text box the mask missed.
    let mut inputs = cleaner_core::text_groups::Inputs::new(width, height, mask.as_deref());
    inputs.page = Some(&page);
    inputs.scope = source_sha256.clone();
    inputs.rt = &boxes;
    inputs.models = capability.split('+').filter_map(|id| match id {
        cleaner_core::cloud_analysis_wire::SAM => Some(ModelUse::new(EvidenceModel::SamTsL, Execution::Cloud, SpatialInput::OverlappingCloudTiles)),
        cleaner_core::cloud_analysis_wire::RT => Some(ModelUse::new(EvidenceModel::OgkaluFull, Execution::Cloud, SpatialInput::OverlappingCloudTiles)),
        _ => None,
    }).collect();
    let (xs, ys) = cleaner_core::cloud_tiles::core_boundaries(width, height);
    inputs.seams = xs.into_iter().map(|x| Seam::Vertical(i64::from(x)))
        .chain(ys.into_iter().map(|y| Seam::Horizontal(i64::from(y)))).collect();
    let evidence = cleaner_core::fusion::fuse_grouped(&inputs, &[])?;
    let mask_sha256 = mask.as_ref().map(|bits| format!("{:x}", Sha256::digest(bits)));
    let id = format!("remote:{provider}:{capability}:{}", &format!("{:x}", Sha256::digest(
        format!("{source_sha256}:{model_identity}:{}", mask_sha256.as_deref().unwrap_or(""))
    ))[..24]);
    let source_data_url = source_preview(source)?;
    let mask_data_url = mask.as_ref().map(|bits| encode_mask(width, height, bits)).transpose()?;
    let candidate_bounds = evidence.components.iter()
        .filter_map(|component| detector_bounds(&evidence, &component.id)
            .map(|bounds| (component.id.clone(), bounds)))
        .collect();
    let bubble_associated_components = evidence.components.iter()
        .filter(|component| !component.rt_bubble_ids.is_empty())
        .map(|component| component.id.clone()).collect();
    let mut store = review_store().lock().map_err(|e| e.to_string())?;
    store.analysis = Some(StoredAnalysis {
        id: id.clone(), sam_backend: "remote", write_eligible: false,
        runtime_sha256: None, source_sha256: source_sha256.clone(),
        model_sha256: model_identity.to_string(), width, height,
        mask: mask.unwrap_or_default(), candidate_bounds,
        bubble_associated_components,
    });
    store.prepared = None;
    Ok(Analysis {
        analysis_id: Some(id), source_sha256,
        rt_model_sha256: None, sam_encoder_sha256: None, sam_head_sha256: None,
        mask_sha256, workflow: capability.to_string(), rt_profile: None,
        // A fused review names both models, joined with `+`.
        rt_backend: capability.split('+').any(|id| id == cleaner_core::cloud_analysis_wire::RT).then_some("remote"),
        sam_backend: capability.split('+').any(|id| id == cleaner_core::cloud_analysis_wire::SAM).then_some("remote"),
        remote_source: Some(format!("remote:{provider}:{capability}")),
        sam_write_eligible: false, sam_webgpu_nodes: None, sam_cpu_fallback_nodes: None,
        sam_process_high_water_bytes: None, sam_process_phys_footprint_bytes: None,
        sam_sampled_metal_high_water_bytes: None,
        evidence, source_data_url, mask_data_url, timings_ms: Timings::default(),
    })
}

fn review_preference(id: &str) -> Result<Preference, String> {
    match id {
        "auto" => Ok(Preference::Automatic),
        "ort-cpu" => Ok(Preference::CpuOnly),
        "ort-coreml" => Ok(Preference::Force(Accelerator::CoreMl)),
        "ort-directml" => Ok(Preference::Force(Accelerator::DirectMl)),
        "ort-cuda" => Ok(Preference::Force(Accelerator::Cuda)),
        "ort-webgpu" => Ok(Preference::Force(Accelerator::WebGpu)),
        _ => Err(format!("Unsupported local review backend {id}")),
    }
}

fn review_backend_id(accelerator: Accelerator) -> &'static str {
    match accelerator {
        Accelerator::Cpu => "ort-cpu",
        Accelerator::CoreMl => "ort-coreml",
        Accelerator::DirectMl => "ort-directml",
        Accelerator::Cuda => "ort-cuda",
        Accelerator::WebGpu => "ort-webgpu",
        _ => "ort-unsupported",
    }
}

#[tauri::command]
pub async fn analyze_capabilities(
    app: tauri::AppHandle,
    source_path: String,
    workflow: String,
    rt_profile: String,
    rt_backend: String,
    sam_backend: String,
    request_id: String,
) -> Result<Analysis, String> {
    let mode = Workflow::parse(&workflow)?;
    let settings = crate::settings::read(&app).unwrap_or(serde_json::Value::Null);
    let rt_id = if rt_profile == "full-halves" { "rtFull" } else { "rtSmall" };
    let mut rt_preference = review_preference(&rt_backend)?;
    if rt_preference == Preference::Automatic {
        rt_preference = crate::run::model_preference_from(&settings, rt_id)?;
    }
    let mut sam_preference = review_preference(&sam_backend)?;
    if sam_preference == Preference::Automatic {
        sam_preference = crate::run::model_preference_from(&settings, "samTs")?;
    }
    let ctd_preference = crate::run::model_preference_from(&settings, "ctd")?;
    let host = cleaner_core::runtime::package::Platform::host();
    for (id, enabled, preference) in [
        (rt_id, mode.rt(), rt_preference),
        ("samTs", mode.sam(), sam_preference),
        ("ctd", mode.ctd(), ctd_preference),
    ] {
        if enabled {
            if let Preference::Force(accelerator) = preference {
                if !crate::models::supports_model(id, accelerator, host) {
                    return Err(format!("{id} does not support {accelerator:?} on this platform"));
                }
            }
        }
    }
    let sam_backend = match sam_preference {
        Preference::Force(Accelerator::WebGpu) => "ort-webgpu".to_owned(),
        Preference::Automatic | Preference::CpuOnly | Preference::Force(Accelerator::Cpu) => "ort-cpu".to_owned(),
        _ if mode.sam() => return Err("The SAM-TS-L lettering mask runs on CPU or Apple WebGPU in local review".into()),
        _ => "ort-cpu".to_owned(),
    };
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let (active_guard, cancel) = begin_analysis(request_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _active_guard = active_guard;
        cancel.check()?;
        let _guard = SAM_FILES.lock().map_err(|e| e.to_string())?;
        cancel.check()?;
        let source = PathBuf::from(source_path);
        let bytes = std::fs::read(&source).map_err(|e| e.to_string())?;
        if bytes.len() > 20_000_000 {
            return Err("Source page exceeds the 20 MB review-preview limit".into());
        }
        let source_sha256 = format!("{:x}", Sha256::digest(&bytes));
        let page = display_source(&bytes)?;
        if u64::from(page.width) * u64::from(page.height) > 24_000_000 {
            return Err("This review preview is limited to 24 megapixels".into());
        }
        cancel.check()?;
        let runtime = cleaner_core::runtime::find(Some(&app_data)).map_err(|e| e.to_string())?;
        cleaner_core::runtime::load(&runtime).map_err(|e| e.to_string())?;
        let mut timings = Timings::default();
        let mut effective_rt_backend = "ort-cpu";
        let mut sam_webgpu_nodes = None;
        let mut sam_cpu_fallback_nodes = None;
        let mut sam_process_high_water_bytes = None;
        let mut sam_process_phys_footprint_bytes = None;
        let mut sam_sampled_metal_high_water_bytes = None;
        let boxes = if mode.rt() {
            match rt_profile.as_str() {
                "full-halves" => {
                    let path = full_rt_path(&app_data)
                        .ok_or("The Ogkalu comic text & bubble detector (Full) graph is not installed or failed SHA-256")?;
                    let (mut model, load) = FullRegions::open(&path, rt_preference)?;
                    effective_rt_backend = review_backend_id(model.selection().accelerator);
                    timings.rt_load = load.as_millis();
                    let started = Instant::now();
                    cancel.check()?;
                    let boxes = model.detect_halves_cancellable(&page, &cancel)?;
                    timings.rt_page = started.elapsed().as_millis();
                    boxes
                }
                "small-whole" => {
                    let path = rt_path(&app_data)
                        .ok_or("The Ogkalu comic text & bubble detector (Small) graph is not installed or failed SHA-256")?;
                    let started = Instant::now();
                    let mut model = BalloonDetector::open(&path, rt_preference)
                        .map_err(|e| e.to_string())?;
                    effective_rt_backend = review_backend_id(model.selection().accelerator);
                    timings.rt_load = started.elapsed().as_millis();
                    let started = Instant::now();
                    cancel.check()?;
                    let boxes = model.detect(&page).map_err(|e| e.to_string())?;
                    cancel.check()?;
                    timings.rt_page = started.elapsed().as_millis();
                    boxes
                }
                _ => return Err("Choose the Full (tiled) or installed Small Ogkalu detector profile".into()),
            }
        } else {
            Vec::new()
        };
        cancel.check()?;
        // CTD boxes stay CTD boxes: grouping gives them their own precedence
        // beside Ogkalu's, as a production run does, and without SAM-TS-L its
        // segmentation is the pixel evidence, as it is there.
        let (ctd_boxes, ctd_mask) = if mode.ctd() {
            let path = crate::run::model_search_paths(Some(&app_data))
                .into_iter()
                .map(|dir| dir.join(crate::run::DETECTOR))
                .find(|path| path.is_file())
                .ok_or("The Comic Text Detector (CTD) graph is not installed")?;
            let mut model = Detector::open(&path, ctd_preference).map_err(|e| e.to_string())?;
            cancel.check()?;
            let detection = model.detect(&page).map_err(|e| e.to_string())?;
            cancel.check()?;
            let seg = (!mode.sam()).then(|| detection.segmentation.lettering_mask());
            (detection.boxes, seg)
        } else {
            (Vec::new(), None)
        };
        cancel.check()?;
        let mask = if mode.sam() {
            cleaner_core::residency::evict_for_sam();
            cancel.check()?;
            if cleaner_core::memory::room().is_none_or(|bytes| bytes < SAM_MIN_ROOM_BYTES) {
                return Err(
                    "The SAM-TS-L lettering mask needs at least 10 GB of measured process memory room"
                        .into(),
                );
            }
            let dir = graph_dir(&app_data).ok_or("The SAM-TS-L lettering mask graphs are not installed")?;
            verify_graphs(&dir)?;
            cancel.check()?;
            let result = match sam_backend.as_str() {
                "ort-cpu" => sam_ts::infer_cpu_cancellable(&page, &dir, &cancel)?,
                "ort-webgpu" => sam_ts::infer_webgpu_cancellable(&page, &dir, &cancel)?,
                _ => return Err("Unsupported SAM backend".into()),
            };
            sam_webgpu_nodes = result.webgpu_nodes;
            sam_cpu_fallback_nodes = result.cpu_fallback_nodes;
            sam_process_high_water_bytes = result.process_high_water_bytes;
            sam_process_phys_footprint_bytes = result.process_phys_footprint_bytes;
            sam_sampled_metal_high_water_bytes = result.sampled_metal_high_water_bytes;
            timings.sam_load = result.load.as_millis();
            timings.sam_prepare = result.prepare.as_millis();
            timings.sam_encoder = result.encoder.as_millis();
            timings.sam_head = result.head.as_millis();
            timings.sam_restore = result.restore.as_millis();
            Some(result.mask)
        } else {
            None
        };
        cancel.check()?;
        let (pixels, pixel_model) = match (&mask, &ctd_mask) {
            (Some(sam), _) => (Some(sam.as_slice()), EvidenceModel::SamTsL),
            (None, Some(seg)) => (Some(seg.as_slice()), EvidenceModel::Ctd),
            (None, None) => (None, EvidenceModel::SamTsL),
        };
        let mut inputs = cleaner_core::text_groups::Inputs::new(page.width, page.height, pixels);
        inputs.pixel_model = pixel_model;
        inputs.page = Some(&page);
        inputs.scope = source_sha256.clone();
        inputs.rt = &boxes;
        inputs.ctd = &ctd_boxes;
        if mode.sam() {
            inputs.models.push(ModelUse::new(EvidenceModel::SamTsL, Execution::Local, SpatialInput::Whole));
        }
        if mode.rt() {
            inputs.models.push(if rt_profile == "full-halves" {
                ModelUse::new(EvidenceModel::OgkaluFull, Execution::Local, SpatialInput::Halves)
            } else {
                ModelUse::new(EvidenceModel::OgkaluSmall, Execution::Local, SpatialInput::Whole)
            });
            if rt_profile == "full-halves" && page.height >= 2 {
                inputs.seams.push(Seam::Horizontal(i64::from(page.height / 2)));
            }
        }
        if mode.ctd() {
            inputs.models.push(ModelUse::new(EvidenceModel::Ctd, Execution::Local, SpatialInput::Whole));
        }
        let evidence = cleaner_core::fusion::fuse_grouped(&inputs, &[])?;
        let mask_sha256 = mask
            .as_ref()
            .map(|mask| format!("{:x}", Sha256::digest(mask)));
        let sam_write_eligible = mode.sam()
            && sam_backend == "ort-webgpu"
            && bytes.starts_with(PNG_MAGIC)
            && validate_write_format(&page).is_ok()
            && qualified_webgpu_write_environment(&app_data)
            && sam_webgpu_nodes.is_some_and(|(encoder, head)| encoder > 0 && head > 0)
            && sam_cpu_fallback_nodes == Some((0, 0));
        let (mask_data_url, source_data_url) = encode_analysis_previews(
            &cancel, page.width, page.height, mask.as_deref(), &bytes
        )?;
        cancel.check()?;
        let analysis_id = mask.as_ref().map(|bits| -> Result<String, String> {
            let model_sha256 = format!("{}:{}", GRAPHS[0].sha256, GRAPHS[1].sha256);
            let id = format!(
                "{:x}",
                Sha256::digest(format!(
                    "analysis-v1:{sam_backend}:{source_sha256}:{model_sha256}:{}",
                    mask_sha256.as_deref().unwrap_or("")
                ))
            );
            commit_analysis(&_active_guard, &cancel, StoredAnalysis {
                id: id.clone(),
                sam_backend: if sam_backend == "ort-webgpu" {
                    "ort-webgpu"
                } else {
                    "ort-cpu"
                },
                write_eligible: sam_write_eligible,
                runtime_sha256: sam_write_eligible.then_some(SAM_WEBGPU_RUNTIME_SHA256),
                source_sha256: source_sha256.clone(),
                model_sha256,
                width: page.width,
                height: page.height,
                mask: bits.clone(),
                candidate_bounds: evidence.components.iter()
                    .filter_map(|component| detector_bounds(&evidence, &component.id).map(|bounds| (component.id.clone(), bounds)))
                    .collect(),
                bubble_associated_components: evidence
                    .components
                    .iter()
                    .filter(|component| !component.rt_bubble_ids.is_empty())
                    .map(|component| component.id.clone())
                    .collect(),
            })?;
            Ok(id)
        }).transpose()?;
        if mask.is_none() {
            let _active = active_analyses().lock().map_err(|e| e.to_string())?;
            cancel.check()?;
            cancel.finish();
        }
        let rt_backend = mode.rt().then_some(effective_rt_backend);
        let sam_backend = mode.sam().then_some(if sam_backend == "ort-webgpu" {
            "ort-webgpu"
        } else {
            "ort-cpu"
        });
        Ok(Analysis {
            analysis_id,
            source_sha256,
            rt_model_sha256: mode.rt().then_some(if rt_profile == "full-halves" {
                FULL_RT_SHA256
            } else {
                RT_SHA256
            }),
            sam_encoder_sha256: mode.sam().then_some(GRAPHS[0].sha256),
            sam_head_sha256: mode.sam().then_some(GRAPHS[1].sha256),
            mask_sha256,
            workflow,
            rt_profile: mode.rt().then_some(rt_profile),
            rt_backend,
            sam_backend,
            remote_source: None,
            sam_write_eligible,
            sam_webgpu_nodes,
            sam_cpu_fallback_nodes,
            sam_process_high_water_bytes,
            sam_process_phys_footprint_bytes,
            sam_sampled_metal_high_water_bytes,
            evidence,
            source_data_url,
            mask_data_url,
            timings_ms: timings,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Analyze the source selected by the chapter rather than accepting a second
/// path from the UI. The returned evidence has the same shape as path-based
/// review and must still pass the source/underlay checks at prepare and apply.
fn chapter_source_path(
    library: &Library,
    chapter_id: &str,
    page_index: usize,
) -> Result<PathBuf, String> {
    let chapter = library
        .resolve_chapter(chapter_id)
        .map_err(|e| e.to_string())?;
    let _job_lock = crate::run::lock_job(&chapter)?;
    let job = Job::open(&chapter).map_err(|e| e.to_string())?;
    if job.project.strip.mode != StripMode::Single {
        return Err("Chapter model analysis currently requires a paginated chapter".into());
    }
    let source_idx =
        Library::resolve_page(&job.project, page_index).ok_or("Page is no longer in chapter")?;
    job.source_path(source_idx)
        .ok_or_else(|| "Chapter source is missing".into())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn analyze_chapter_page(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: usize,
    workflow: String,
    rt_profile: String,
    rt_backend: String,
    sam_backend: String,
    request_id: String,
) -> Result<Analysis, String> {
    let library = Library::for_app(&app).map_err(|e| e.to_string())?;
    let source_path = chapter_source_path(&library, &chapter_id, page_index)?
        .to_str()
        .ok_or("Chapter source path is not UTF-8")?
        .to_owned();
    analyze_capabilities(
        app,
        source_path,
        workflow,
        rt_profile,
        rt_backend,
        sam_backend,
        request_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    #[test]
    fn review_source_preview_is_managed_srgb_and_oriented_once() {
        use base64::Engine as _;
        let fixture=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/cleaner-core/tests/fixtures/color-reference");
        let bytes=std::fs::read(fixture.join("orientation-6.jpg")).unwrap();
        let native=cleaner_core::image::decode(&bytes).unwrap();
        let shown=super::display_source(&bytes).unwrap();
        assert_eq!((shown.width,shown.height),(native.height,native.width));
        let bytes=std::fs::read(fixture.join("gamma-chrm.png")).unwrap();
        let preview=super::source_preview(&bytes).unwrap();
        let encoded=base64::engine::general_purpose::STANDARD.decode(preview.strip_prefix("data:image/png;base64,").unwrap()).unwrap();
        let shown=cleaner_core::image::decode(&encoded).unwrap();
        assert_eq!(shown.srgb_intent,Some(1));
        assert_eq!(shown.color.gamma,None);
        // Linear red sample 32/255 in the independent fixture encodes to99.
        let source=cleaner_core::image::decode(&bytes).unwrap();
        let expected=(if source.sample(0,0,0) as f64/255.0<=0.0031308 {source.sample(0,0,0) as f64/255.0*12.92} else {1.055*(source.sample(0,0,0) as f64/255.0).powf(1.0/2.4)-0.055})*255.0;
        assert!((shown.sample(0,0,0) as f64-expected).abs()<1.5);
    }

    #[test]
    fn bounded_fill_keeps_native_metadata_alpha_and_hidden_samples() {
        let mut image = cleaner_core::image::fixtures::by_name("rgba8").raster;
        image.color.gamma = Some(100000);
        for y in 0..image.height { for x in 0..image.width {
            for c in 0..3 { image.set_sample(x,y,c,80); }
            image.set_sample(x,y,3,200);
        }}
        for c in 0..3 { image.set_sample(21,21,c,12); image.set_sample(22,22,c,19); }
        image.set_sample(21,21,3,0);
        image.set_sample(22,22,3,128);
        let support=cleaner_core::mask::Mask::filled(cleaner_core::mask::Rect::new(20,20,5,5));
        let input=crate::underlay::Input { window: cleaner_core::mask::Rect::new(0,0,image.width,image.height), image, digest:String::new(),predecessors:String::new() };
        let out=super::render_bounded_fill(&input,&support).unwrap();
        assert_eq!(out.icc,input.image.icc);
        assert_eq!(out.color,input.image.color);
        assert_eq!(out.srgb_intent,input.image.srgb_intent);
        for c in 0..3 { assert_eq!(out.sample(1,1,c),12); assert_eq!(out.sample(2,2,c),80); }
        assert_eq!(out.sample(1,1,3),0);
        assert_eq!(out.sample(2,2,3),128);
        let mut indexed=cleaner_core::image::fixtures::by_name("indexed-p").raster;
        indexed.trns=Some(vec![0,64,128,255]);
        for y in 0..indexed.height { for x in 0..indexed.width { indexed.set_sample(x,y,0,1); }}
        indexed.set_sample(22,22,0,2);
        let input=crate::underlay::Input { window: cleaner_core::mask::Rect::new(0,0,indexed.width,indexed.height), image:indexed,digest:String::new(),predecessors:String::new() };
        let out=super::render_bounded_fill(&input,&support).unwrap();
        assert_eq!(out.sample(2,2,0),2,"native palette alpha must not change");
    }

    use super::*;

    /// The review store is one global. Tests that put an analysis in it run
    /// one at a time, or one replaces another's analysis while it waits.
    fn store_serial() -> std::sync::MutexGuard<'static, ()> {
        static STORE: Mutex<()> = Mutex::new(());
        STORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn local_review_backend_ids_map_to_strict_preferences() {
        assert_eq!(review_preference("auto").unwrap(), Preference::Automatic);
        assert_eq!(review_preference("ort-cpu").unwrap(), Preference::CpuOnly);
        assert_eq!(review_preference("ort-webgpu").unwrap(), Preference::Force(Accelerator::WebGpu));
        assert_eq!(review_preference("ort-directml").unwrap(), Preference::Force(Accelerator::DirectMl));
        assert!(review_preference("mlx").is_err());
        assert_eq!(review_backend_id(Accelerator::Cuda), "ort-cuda");
    }

    #[test]
    fn status_digest_cache_rehashes_changed_mtime_and_size() {
        use std::cell::Cell;
        use std::fs::{FileTimes, OpenOptions};
        use std::time::Duration;

        let path = std::env::temp_dir().join(format!(
            "mc-status-digest-{}-{:?}", std::process::id(), std::thread::current().id()
        ));
        let file = OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
        std::fs::write(&path, b"first").unwrap();
        let expected = format!("{:x}", Sha256::digest(b"first"));
        let hashes = Cell::new(0);
        let hash = |path: &Path| {
            hashes.set(hashes.get() + 1);
            digest(path)
        };
        let mut cache = StatusDigestCache::default();
        assert!(cache.verified_with(&path, 5, &expected, hash));
        assert!(cache.verified_with(&path, 5, &expected, hash));
        assert_eq!(hashes.get(), 1);

        let modified = file.metadata().unwrap().modified().unwrap() + Duration::from_secs(60);
        file.set_times(FileTimes::new().set_modified(modified)).unwrap();
        assert!(cache.verified_with(&path, 5, &expected, hash));
        assert_eq!(hashes.get(), 2);

        std::fs::write(&path, b"second").unwrap();
        assert!(!cache.verified_with(&path, 6, &expected, hash));
        assert_eq!(hashes.get(), 3);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn workflow_status_keeps_parked_sessions() {
        use cleaner_core::registry::Kind;
        use cleaner_core::residency::{self, Key, Resident};

        struct Parked;
        impl Resident for Parked {
            fn spent(&self) -> bool {
                false
            }
        }

        let key = Key::new(Kind::TextDetector, format!(
            "workflow-status-{}-{:?}", std::process::id(), std::thread::current().id()
        ));
        residency::checkin(key.clone(), Parked);
        let status = workflow_capabilities_with_room(&std::env::temp_dir(), Some(0));
        // Short on room, but the parked session is room analysis can make.
        assert!(status.sam_memory_ready);
        assert!(residency::checkout::<Parked>(&key).is_some());
    }

    fn store_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn remote_evidence_cannot_prepare_or_apply_a_component_write() {
        let _store = store_serial();
        let _guard = store_test_lock();
        let mut source = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut source, 16, 16);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap().write_image_data(&vec![255; 16 * 16 * 3]).unwrap();
        }
        let mut mask = vec![0; 16 * 16];
        mask[0] = 255;
        let analysis = store_remote_analysis("modal", cleaner_core::cloud_analysis_wire::SAM,
            &"a".repeat(64), &source, 16, 16, Some(mask), vec![]).unwrap();
        let id = analysis.analysis_id.unwrap();
        let library = Library::at(std::env::temp_dir().join("mc-remote-write-refusal"));
        let result = prepare_component_at(&library, true, &id, "chapter", 0, "sam-00001",
            false, 0, empty_correction(), empty_correction(), 0);
        assert!(result.err().unwrap().contains("review-only"));
        assert!(review_store().lock().unwrap().prepared.is_none());
        assert!(apply_component_at(&library, true, "remote-plan", "approved-support").is_err());
    }

    /// The cloud preview groups the way a cloud run does: an Ogkalu text box
    /// with no mask under it gets the run's bounded ink estimate, which needs
    /// the page's pixels.
    #[test]
    fn a_remote_ogkalu_preview_estimates_lettering_as_a_run_does() {
        let _store = store_serial();
        let _guard = store_test_lock();
        let (width, height) = (64u32, 48u32);
        let mut pixels = vec![255u8; (width * height * 3) as usize];
        for y in [12usize, 18, 24] {
            for x in 12..40usize {
                for c in 0..3 {
                    pixels[(y * width as usize + x) * 3 + c] = 20;
                }
            }
        }
        let mut source = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut source, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
        }
        let boxes = vec![cleaner_core::balloon::BalloonBox {
            rect: cleaner_core::mask::Rect::new(8, 8, 40, 22),
            class: cleaner_core::balloon::BalloonClass::TextFree,
            score: 0.9,
        }];
        let analysis = store_remote_analysis("modal", cleaner_core::cloud_analysis_wire::RT,
            &"b".repeat(64), &source, width, height, None, boxes).unwrap();
        let groups = &analysis.evidence.groups;
        assert_eq!(groups.len(), 1);
        assert!(groups[0].estimated, "the preview left the box without its estimate");
        assert_eq!(groups[0].lettering_pixels, 3 * 28);
    }

    #[test]
    fn managed_sam_removal_hides_both_graphs_together() {
        let models = std::env::temp_dir()
            .join(format!("mc-sam-removal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&models);
        let managed = models.join("sam-ts-l");
        std::fs::create_dir_all(&managed).unwrap();
        for graph in GRAPHS {
            std::fs::write(managed.join(graph.name), b"fixture").unwrap();
        }
        assert!(remove_managed_sam_dir(&models).unwrap());
        assert!(!managed.exists());
        assert!(!remove_managed_sam_dir(&models).unwrap());
        std::fs::remove_dir_all(models).unwrap();
    }

    #[test]
    fn cancel_before_and_between_stages_never_commits_review_evidence() {
        let _store = store_serial();
        let _guard = store_test_lock();
        let request = format!("cancel-test-{}", std::process::id());
        let (active, cancel) = begin_analysis(request.clone()).unwrap();
        let previous = review_store().lock().unwrap().analysis.as_ref().map(|a| a.id.clone());
        let fixture = || StoredAnalysis {
            id: "cancelled-analysis".into(), sam_backend: "ort-cpu", write_eligible: false,
            runtime_sha256: None, source_sha256: "source".into(), model_sha256: "model".into(),
            width: 1, height: 1, mask: vec![255],
            bubble_associated_components: std::collections::HashSet::new(),
            candidate_bounds: HashMap::new(),
        };
        assert!(cancel_capability_analysis(request.clone()).unwrap());
        assert_eq!(cancel.check().unwrap_err(), "analysis cancelled");
        assert_eq!(commit_analysis(&active, &cancel, fixture()).unwrap_err(), "analysis cancelled");
        drop(active);
        let (active, cancel) = begin_analysis(format!("{request}-next")).unwrap();
        cancel.check().unwrap(); // decoded page
        cancel.check().unwrap(); // detector result
        assert!(cancel_capability_analysis(format!("{request}-next")).unwrap());
        assert_eq!(cancel.check().unwrap_err(), "analysis cancelled"); // before SAM
        assert_eq!(commit_analysis(&active, &cancel, fixture()).unwrap_err(), "analysis cancelled");
        assert_eq!(review_store().lock().unwrap().analysis.as_ref().map(|a| a.id.clone()), previous);
    }

    #[test]
    fn cancel_after_fusion_skips_previews_and_commit() {
        let _store = store_serial();
        let _guard = store_test_lock();
        let request = format!("cancel-after-fusion-{}", std::process::id());
        let (active, cancel) = begin_analysis(request.clone()).unwrap();
        let previous = review_store().lock().unwrap().analysis.as_ref().map(|a| a.id.clone());
        let _evidence = cleaner_core::fusion::fuse(2, 2, None, &[], &[]).unwrap();
        assert!(cancel_capability_analysis(request).unwrap());
        assert_eq!(
            encode_analysis_previews(&cancel, 2, 2, Some(&[255]), b"unsupported")
                .err().unwrap(),
            "analysis cancelled"
        );
        let analysis = StoredAnalysis {
            id: "after-fusion-analysis".into(), sam_backend: "ort-cpu", write_eligible: false,
            runtime_sha256: None, source_sha256: "source".into(), model_sha256: "model".into(),
            width: 2, height: 2, mask: vec![255; 4],
            bubble_associated_components: std::collections::HashSet::new(),
            candidate_bounds: HashMap::new(),
        };
        assert_eq!(commit_analysis(&active, &cancel, analysis).unwrap_err(), "analysis cancelled");
        assert_eq!(review_store().lock().unwrap().analysis.as_ref().map(|a| a.id.clone()), previous);
    }

    #[test]
    fn completed_request_id_cannot_cancel_a_later_analysis() {
        let request = format!("completed-analysis-{}", std::process::id());
        let (active, cancel) = begin_analysis(request.clone()).unwrap();
        cancel.finish();
        drop(active);
        assert!(begin_analysis(request.clone()).err().unwrap().contains("already used"));
        let (later, later_cancel) = begin_analysis(format!("{request}-next")).unwrap();
        assert!(!cancel_capability_analysis(request).unwrap());
        later_cancel.check().unwrap();
        drop(later);
    }

    #[test]
    fn indexed_and_sub_byte_sources_are_refused_for_component_fill() {
        use cleaner_core::image::fixtures;
        let indexed = fixtures::by_name("indexed-p").raster;
        let bitonal = fixtures::by_name("bitonal").raster;
        assert!(validate_write_format(&indexed).unwrap_err().contains("indexed palette"));
        assert!(validate_write_format(&bitonal).unwrap_err().contains("sub-8-bit"));
        for name in ["rgb8", "rgba8", "l16", "rgb16"] {
            validate_write_format(&fixtures::by_name(name).raster).unwrap();
        }
    }

    #[test]
    fn prepare_refuses_indexed_and_sub_byte_png_sources() {
        let _store = store_serial();
        use cleaner_core::image::{encode, fixtures, Format};

        let _guard = store_test_lock();
        for (name, message) in [("indexed-p", "indexed palette"), ("bitonal", "sub-8-bit")] {
            let root = std::env::temp_dir().join(format!("mc-narrow-source-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let scans = root.join("scans");
            std::fs::create_dir_all(&scans).unwrap();
            let source = fixtures::by_name(name).raster;
            let bytes = encode(&source, Format::Png).unwrap();
            std::fs::write(scans.join("001.png"), &bytes).unwrap();
            let library = Library::at(root.join("library"));
            let project = library.create_project(name, StripMode::Single, Some(scans.clone()), None).unwrap();
            let chapter = library.create_chapter(&project.id, "chapter", None, Some(scans)).unwrap().created().unwrap();
            let mut mask = vec![0; source.width as usize * source.height as usize];
            mask[10 * source.width as usize + 10] = 255;
            let analysis_id = format!("narrow-{name}");
            {
                let mut store = review_store().lock().unwrap();
                store.analysis = Some(StoredAnalysis {
                    id: analysis_id.clone(), sam_backend: "ort-webgpu", write_eligible: true,
                    runtime_sha256: Some(SAM_WEBGPU_RUNTIME_SHA256),
                    source_sha256: format!("{:x}", Sha256::digest(&bytes)),
                    model_sha256: "fixture-model".into(), width: source.width, height: source.height,
                    mask, bubble_associated_components: std::collections::HashSet::new(),
                    candidate_bounds: HashMap::new(),
                });
                store.prepared = None;
            }
            let error = prepare_component_at(&library, true, &analysis_id, &chapter.id, 0,
                "sam-00001", true, 0, empty_correction(), empty_correction(), 0).err().unwrap();
            assert!(error.contains(message), "{name}: {error}");
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn associated_detector_bounds_are_kept_as_the_candidate_locator() {
        use cleaner_core::balloon::{BalloonBox, BalloonClass};
        let mut mask = vec![0; 20 * 20];
        mask[7 * 20 + 7] = 255;
        let boxes = [BalloonBox { rect: Rect::new(4, 4, 8, 8), class: BalloonClass::TextFree, score: 0.9 }];
        let evidence = cleaner_core::fusion::fuse(20, 20, Some(&mask), &boxes, &[]).unwrap();
        assert_eq!(detector_bounds(&evidence, "sam-00001"), Some(boxes[0].rect));
        assert_eq!(evidence.components[0].bounds, Rect::new(7, 7, 1, 1));
        assert_eq!(detector_bounds(&evidence, "missing"), None);
        let bubble = [BalloonBox { rect: Rect::new(3, 3, 10, 10), class: BalloonClass::Bubble, score: 0.7 }];
        let context = cleaner_core::fusion::fuse(20, 20, Some(&mask), &bubble, &[]).unwrap();
        assert_eq!(detector_bounds(&context, "sam-00001"), Some(bubble[0].rect));
    }

    #[test]
    fn each_workflow_opens_only_its_requested_models() {
        let mask = Workflow::parse("mask").unwrap();
        assert_eq!((mask.ctd(), mask.rt(), mask.sam()), (false, false, true));
        assert_eq!(
            (Workflow::parse("regions").unwrap().rt(), Workflow::parse("regions").unwrap().sam()),
            (true, false)
        );
        assert_eq!(
            (Workflow::parse("text_shape").unwrap().rt(), Workflow::parse("text_shape").unwrap().sam()),
            (true, true)
        );
        for (name, ctd, rt, sam) in [
            ("ctd", true, false, false), ("ctd_regions", true, true, false),
            ("ctd_mask", true, false, true), ("ctd_text_shape", true, true, true),
        ] {
            let workflow = Workflow::parse(name).unwrap();
            assert_eq!((workflow.ctd(), workflow.rt(), workflow.sam()), (ctd, rt, sam));
        }
        assert!(Workflow::parse("rtdetr-coo-samts").is_err());
        assert_eq!(source_mime(PNG_MAGIC).unwrap(), "image/png");
        assert_eq!(source_mime(&[0xff, 0xd8, 0xff]).unwrap(), "image/jpeg");
        assert!(source_mime(b"not an image").is_err());
    }

    #[test]
    #[ignore = "requires local SAM graphs, app ORT WebGPU runtime, and measured M5 host"]
    fn real_webgpu_mask_commits_only_approved_support() {
        let _store = store_serial();
        use cleaner_core::composite::changed_pixels;
        use cleaner_core::export::{export_page, Target};
        use cleaner_core::image::decode;

        let runtime = std::env::var("SAM_TS_RUNTIME").expect("SAM_TS_RUNTIME is required");
        cleaner_core::runtime::load(Path::new(&runtime)).unwrap();
        let spike = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spikes/sam-ts-l");
        let source_bytes = std::fs::read(spike.join("fixtures/synthetic_page.png")).unwrap();
        let page = decode(&source_bytes).unwrap();
        let inference = sam_ts::infer_webgpu(&page, &spike.join("artifacts/proof")).unwrap();
        assert_eq!(inference.cpu_fallback_nodes, Some((0, 0)));
        let evidence = cleaner_core::fusion::fuse(
            page.width,
            page.height,
            Some(&inference.mask),
            &[],
            &[],
        )
        .unwrap();
        let component = evidence
            .components
            .iter()
            .max_by_key(|component| component.pixels)
            .expect("reference mask must contain lettering");
        let root = std::env::temp_dir().join(format!("mc-real-webgpu-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let scans = root.join("scans");
        std::fs::create_dir_all(&scans).unwrap();
        std::fs::write(scans.join("001.png"), &source_bytes).unwrap();
        let library = Library::at(root.join("library"));
        let project = library
            .create_project("real-webgpu", StripMode::Single, Some(scans.clone()), None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "chapter", None, Some(scans))
            .unwrap()
            .created()
            .unwrap();
        let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
        let model_sha256 = format!("{}:{}", GRAPHS[0].sha256, GRAPHS[1].sha256);
        {
            let mut store = review_store().lock().unwrap();
            store.analysis = Some(StoredAnalysis {
                id: "real-webgpu-analysis".into(),
                sam_backend: "ort-webgpu",
                write_eligible: true,
                runtime_sha256: Some(SAM_WEBGPU_RUNTIME_SHA256),
                source_sha256,
                model_sha256,
                width: page.width,
                height: page.height,
                mask: inference.mask,
                bubble_associated_components: std::collections::HashSet::new(),
                candidate_bounds: HashMap::new(),
            });
            store.prepared = None;
        }
        let prepared = prepare_component_at(
            &library,
            true,
            "real-webgpu-analysis",
            &chapter.id,
            0,
            &component.id,
            true,
            0,
            empty_correction(),
            empty_correction(),
            0,
        )
        .unwrap();
        let applied = apply_component_at(
            &library,
            true,
            &prepared.plan_id,
            &prepared.support_sha256,
        )
        .unwrap();
        let job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
        let record = job
            .project
            .patches
            .iter()
            .find(|record| record.id == applied.region_id)
            .unwrap();
        let patch = job.load_patch(record).unwrap();
        assert_eq!(patch.mask.count(), prepared.support_pixels);
        let exported = export_page(&source_bytes, std::slice::from_ref(&patch), Target::SameAsSource).unwrap();
        let changed = changed_pixels(&page, &decode(&exported.bytes).unwrap());
        assert!(!changed.is_empty());
        assert!(changed.iter().all(|(x, y)| patch.mask.contains(*x as i64, *y as i64)));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn component_export_changes_only_support_in_direct_png_modes() {
        let _store = store_serial();
        use cleaner_core::composite::changed_pixels;
        use cleaner_core::export::{export_page, Target};
        use cleaner_core::image::{decode, encode, fixtures, Format};

        let _guard = store_test_lock();
        for name in ["rgb8", "rgba8", "l16", "rgb16"] {
            let root = std::env::temp_dir().join(format!("mc-component-modes-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let scans = root.join("scans");
            std::fs::create_dir_all(&scans).unwrap();
            let mut source = fixtures::by_name(name).raster;
            source.set_sample(10, 10, 0, 0);
            if name == "rgba8" { source.set_sample(10, 10, 3, 96); }
            let bytes = encode(&source, Format::Png).unwrap();
            std::fs::write(scans.join("001.png"), &bytes).unwrap();
            let library = Library::at(root.join("library"));
            let project = library.create_project(name, StripMode::Single, Some(scans.clone()), None).unwrap();
            let chapter = library.create_chapter(&project.id, "chapter", None, Some(scans)).unwrap().created().unwrap();
            let mut mask = vec![0; source.width as usize * source.height as usize];
            mask[10 * source.width as usize + 10] = 255;
            let analysis_id = format!("mode-{name}");
            {
                let mut store = review_store().lock().unwrap();
                store.analysis = Some(StoredAnalysis {
                    id: analysis_id.clone(), sam_backend: "ort-webgpu", write_eligible: true,
                    runtime_sha256: Some(SAM_WEBGPU_RUNTIME_SHA256),
                    source_sha256: format!("{:x}", Sha256::digest(&bytes)),
                    model_sha256: "fixture-model".into(), width: source.width, height: source.height,
                    mask, bubble_associated_components: std::collections::HashSet::new(),
                    candidate_bounds: HashMap::new(),
                });
                store.prepared = None;
            }
            let prepare = |padding_px| prepare_component_at(
                &library, true, &analysis_id, &chapter.id, 0, "sam-00001", true,
                padding_px, empty_correction(), empty_correction(), 0,
            ).unwrap();
            let five = prepare(5);
            let five_support = review_store().lock().unwrap().prepared.as_ref().unwrap().support.clone();
            apply_component_at(&library, true, &five.plan_id, &five.support_sha256).unwrap();
            let export = || {
                let job = Job::open(&library.resolve_chapter(&chapter.id).unwrap()).unwrap();
                let patches: Vec<_> = job.project.patches.iter().map(|p| job.load_patch(p).unwrap()).collect();
                decode(&export_page(&bytes, &patches, Target::SameAsSource).unwrap().bytes).unwrap()
            };
            let after_five = export();
            let changed = changed_pixels(&source, &after_five);
            assert!(!changed.is_empty(), "{name}");
            assert!(changed.iter().all(|(x, y)| five_support.contains(*x as i64, *y as i64)), "{name}");
            let two = prepare(2);
            let two_support = review_store().lock().unwrap().prepared.as_ref().unwrap().support.clone();
            apply_component_at(&library, true, &two.plan_id, &two.support_sha256).unwrap();
            let after_two = export();
            assert!(changed_pixels(&source, &after_two).iter().all(|(x, y)| two_support.contains(*x as i64, *y as i64)), "{name}");
            for y in five_support.bounds.y..five_support.bounds.bottom() {
                for x in five_support.bounds.x..five_support.bounds.right() {
                    if five_support.contains(x, y) && !two_support.contains(x, y) {
                        for channel in 0..source.mode.samples() {
                            assert_eq!(after_two.sample(x as u32, y as u32, channel), source.sample(x as u32, y as u32, channel), "{name} ({x}, {y}) channel {channel}");
                        }
                    }
                }
            }
            if name == "rgba8" {
                for y in 0..source.height {
                    for x in 0..source.width {
                        assert_eq!(after_two.sample(x, y, 3), source.sample(x, y, 3));
                    }
                }
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn exact_support_survives_manifest_export_and_persisted_undo_redo() {
        let _store = store_serial();
        let _guard = store_test_lock();
        use cleaner_core::composite::changed_pixels;
        use cleaner_core::export::{Target, export_page};
        use cleaner_core::image::{Format, decode, encode, fixtures};
        use cleaner_core::project::StripMode;

        let root = std::env::temp_dir().join(format!("mc-component-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let scans = root.join("scans");
        std::fs::create_dir_all(&scans).unwrap();
        let mut raw = fixtures::by_name("l8").raster;
        raw.data.fill(255);
        raw.set_sample(30, 20, 0, 0);
        raw.set_sample(31, 20, 0, 0);
        raw.set_sample(40, 20, 0, 0);
        std::fs::write(scans.join("001.png"), encode(&raw, Format::Png).unwrap()).unwrap();
        let library = Library::at(root.join("library"));
        let project = library
            .create_project("test", StripMode::Single, Some(scans.clone()), None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "chapter", None, Some(scans))
            .unwrap()
            .created()
            .unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let mut job = Job::open(&path).unwrap();
        assert_eq!(
            chapter_source_path(&library, &chapter.id, 0).unwrap(),
            job.source_path(0).unwrap()
        );
        assert!(chapter_source_path(&library, &chapter.id, 1).is_err());
        let source_bytes = std::fs::read(job.source_path(0).unwrap()).unwrap();
        let source_sha256 = format!("{:x}", Sha256::digest(&source_bytes));
        let mut bits = vec![0; raw.width as usize * raw.height as usize];
        bits[20 * raw.width as usize + 30] = 255;
        bits[20 * raw.width as usize + 31] = 255;
        let analysis_id = "synthetic-analysis";
        {
            let mut store = review_store().lock().unwrap();
            store.analysis = Some(StoredAnalysis {
                id: analysis_id.into(),
                sam_backend: "ort-webgpu",
                write_eligible: true,
                runtime_sha256: Some(SAM_WEBGPU_RUNTIME_SHA256),
                source_sha256,
                model_sha256: "synthetic-model".into(),
                width: raw.width,
                height: raw.height,
                mask: bits,
                bubble_associated_components: std::collections::HashSet::new(),
                candidate_bounds: HashMap::from([("sam-00001".into(), Rect::new(25, 15, 20, 16))]),
            });
            store.prepared = None;
        }

        assert!(
            prepare_component_at(
                &library,
                false,
                analysis_id,
                &chapter.id,
                0,
                "sam-00001",
                true,
                0,
                empty_correction(),
                empty_correction(),
                0
            )
            .err()
            .unwrap()
            .contains("not qualified")
        );
        {
            let mut store = review_store().lock().unwrap();
            store.analysis.as_mut().unwrap().sam_backend = "ort-cpu";
        }
        assert!(
            prepare_component_at(
                &library,
                true,
                analysis_id,
                &chapter.id,
                0,
                "sam-00001",
                true,
                0,
                empty_correction(),
                empty_correction(),
                0
            )
            .err()
            .unwrap()
            .contains("not qualified")
        );
        review_store()
            .lock()
            .unwrap()
            .analysis
            .as_mut()
            .unwrap()
            .sam_backend = "ort-webgpu";
        assert!(
            prepare_component_at(
                &library,
                true,
                analysis_id,
                &chapter.id,
                0,
                "sam-00001",
                false,
                0,
                empty_correction(),
                empty_correction(),
                0,
            )
            .err()
            .unwrap()
            .contains("held")
        );
        let padded_two = prepare_component_at(
            &library,
            true,
            analysis_id,
            &chapter.id,
            0,
            "sam-00001",
            true,
            2,
            empty_correction(),
            empty_correction(),
            0,
        )
        .unwrap();
        let padded_five = prepare_component_at(
            &library,
            true,
            analysis_id,
            &chapter.id,
            0,
            "sam-00001",
            true,
            5,
            empty_correction(),
            empty_correction(),
            0,
        )
        .unwrap();
        let padded_two_again = prepare_component_at(
            &library,
            true,
            analysis_id,
            &chapter.id,
            0,
            "sam-00001",
            true,
            2,
            empty_correction(),
            empty_correction(),
            0,
        )
        .unwrap();
        assert_eq!(padded_two.support_sha256, padded_two_again.support_sha256);
        assert_eq!(
            padded_two.support_data_url,
            padded_two_again.support_data_url
        );
        assert!(padded_five.support_pixels > padded_two.support_pixels);
        assert_ne!(padded_two.plan_id, padded_five.plan_id);
        let mut addition = Mask::empty(Rect::new(40, 20, 1, 1));
        addition.set(40, 20, true);
        let mut removal = Mask::empty(Rect::new(30, 20, 1, 1));
        removal.set(30, 20, true);
        let corrected = prepare_component_at(
            &library,
            true,
            analysis_id,
            &chapter.id,
            0,
            "sam-00001",
            true,
            0,
            addition.into(),
            removal.into(),
            1,
        )
        .unwrap();
        assert_eq!(corrected.support_pixels, 2);
        assert_ne!(corrected.support_sha256, padded_two.support_sha256);
        let stale = prepare_component_at(
            &library,
            true,
            analysis_id,
            &chapter.id,
            0,
            "sam-00001",
            true,
            0,
            empty_correction(),
            empty_correction(),
            0,
        )
        .unwrap();
        let a_mask = Mask::filled(Rect::new(10, 10, 4, 4));
        let mut a_pixels = raw.clone();
        a_pixels.width = 4;
        a_pixels.height = 4;
        a_pixels.data = vec![180; 16];
        let a = Patch {
            id: format!("{}-r0", crate::library::page_id(&chapter.id, 0)),
            mask: a_mask.clone(),
            ink: a_mask,
            pixels: a_pixels,
            order: 0,
            visible: true,
            provenance: Provenance {
                engine: PatchEngine::Fill,
                engine_version: "test".into(),
                model_sha256: None,
                execution_provider: "cpu".into(),
                params_snapshot: serde_json::json!({}),
                mask_sha256: "fixture".into(),
                source_sha256: "fixture".into(),
                cloud: None,
                created: 0,
            },
        };
        job.complete_region(0, &a, None).unwrap();
        let stale_error = apply_component_at(&library, true, &stale.plan_id, &stale.support_sha256)
            .err()
            .unwrap();
        assert!(stale_error.contains("since preview"), "{stale_error}");

        let mut addition = Mask::empty(Rect::new(40, 20, 1, 1));
        addition.set(40, 20, true);
        let mut removal = Mask::empty(Rect::new(30, 20, 1, 1));
        removal.set(30, 20, true);
        let prepared = prepare_component_at(
            &library,
            true,
            analysis_id,
            &chapter.id,
            0,
            "sam-00001",
            true,
            0,
            addition.into(),
            removal.into(),
            1,
        )
        .unwrap();
        assert!(
            apply_component_at(&library, false, &prepared.plan_id, &prepared.support_sha256)
                .err()
                .unwrap()
                .contains("not qualified")
        );
        {
            let mut store = review_store().lock().unwrap();
            store.analysis.as_mut().unwrap().sam_backend = "ort-cpu";
        }
        assert!(
            apply_component_at(&library, true, &prepared.plan_id, &prepared.support_sha256)
                .err()
                .unwrap()
                .contains("Analysis changed")
        );
        {
            let mut store = review_store().lock().unwrap();
            store.analysis.as_mut().unwrap().sam_backend = "ort-webgpu";
            store.prepared.as_mut().unwrap().runtime_sha256 = "different-runtime";
        }
        assert!(
            apply_component_at(&library, true, &prepared.plan_id, &prepared.support_sha256)
                .err()
                .unwrap()
                .contains("not qualified")
        );
        review_store()
            .lock()
            .unwrap()
            .prepared
            .as_mut()
            .unwrap()
            .runtime_sha256 = SAM_WEBGPU_RUNTIME_SHA256;
        assert_ne!(prepared.underlay_sha256, stale.underlay_sha256);
        assert_eq!(prepared.support_pixels, 2);
        let before_job = Job::open(&path).unwrap();
        let a_record = before_job
            .project
            .patches
            .iter()
            .find(|p| p.id == a.id)
            .unwrap();
        let baseline = export_page(
            &source_bytes,
            &[before_job.load_patch(a_record).unwrap()],
            Target::SameAsSource,
        )
        .unwrap();
        let applied =
            apply_component_at(&library, true, &prepared.plan_id, &prepared.support_sha256)
                .unwrap();
        let after_job = Job::open(&path).unwrap();
        let patches: Vec<_> = after_job
            .project
            .patches
            .iter()
            .map(|p| after_job.load_patch(p).unwrap())
            .collect();
        let exported = export_page(&source_bytes, &patches, Target::SameAsSource).unwrap();
        let before_pixels = decode(&baseline.bytes).unwrap();
        let after_pixels = decode(&exported.bytes).unwrap();
        let (saved_plan, saved_prepared) = after_job
            .load_text_shape_plan(&applied.region_id)
            .unwrap()
            .unwrap();
        let saved_correction = load_component_correction_at(
            &library,
            analysis_id,
            &chapter.id,
            0,
            "sam-00001",
        )
        .unwrap()
        .unwrap();
        assert_eq!(saved_correction.region_id, applied.region_id);
        assert_eq!(saved_correction.correction_revision, 1);
        assert_eq!(saved_correction.additions.count(), 1);
        assert_eq!(saved_correction.removals.count(), 1);
        assert_eq!(saved_plan.candidate_bounds, Rect::new(25, 15, 20, 16));
        assert_eq!(saved_plan.correction_revision, 1);
        assert_eq!(saved_plan.additions.count(), 1);
        assert_eq!(saved_plan.removals.count(), 1);
        let support = saved_prepared.write_support.to_mask();
        let changed = changed_pixels(&before_pixels, &after_pixels);
        assert!(!changed.is_empty());
        assert_eq!(after_pixels.sample(30, 20, 0), 0);
        assert_eq!(
            after_job.project.patches.last().unwrap().geometry_policy,
            cleaner_core::text_shape::GeometryPolicy::TextShape
        );
        assert!(
            changed
                .iter()
                .all(|&(x, y)| support.contains(x as i64, y as i64)),
            "export changed pixels outside approved support: {changed:?}"
        );
        assert_eq!(
            after_job
                .project
                .patches
                .last()
                .unwrap()
                .provenance
                .params_snapshot["underlay_sha256"]
                .as_str(),
            Some(prepared.underlay_sha256.as_str())
        );

        let mut journal = crate::history::load(&path);
        journal.push(crate::history::NewDelta {
            label: "canvas.command.applyTool".into(),
            op: "region-state".into(),
            region_id: applied.region_id.clone(),
            before: crate::history::Side {
                present: false,
                page_status: None,
                region: None,
            },
            after: crate::history::Side {
                present: true,
                page_status: Some(applied.page_status),
                region: Some(serde_json::to_value(applied.region).unwrap()),
            },
        });
        crate::history::save(&path, &journal).unwrap();
        let mut reopened = crate::history::load(&path);
        assert_eq!(reopened.cursor, 1);
        assert_eq!(reopened.undo().unwrap().region_id, applied.region_id);
        crate::history::save(&path, &reopened).unwrap();
        library.set_mask_visible(&applied.region_id, false).unwrap();
        let undone = Job::open(&path).unwrap();
        let undone_patches: Vec<_> = undone
            .project
            .patches
            .iter()
            .map(|p| undone.load_patch(p).unwrap())
            .collect();
        assert_eq!(
            export_page(&source_bytes, &undone_patches, Target::SameAsSource)
                .unwrap()
                .bytes,
            baseline.bytes
        );
        let mut reopened = crate::history::load(&path);
        assert_eq!(reopened.cursor, 0);
        assert_eq!(reopened.redo().unwrap().region_id, applied.region_id);
        crate::history::save(&path, &reopened).unwrap();
        library.set_mask_visible(&applied.region_id, true).unwrap();
        let redone = Job::open(&path).unwrap();
        let redone_patches: Vec<_> = redone
            .project
            .patches
            .iter()
            .map(|p| redone.load_patch(p).unwrap())
            .collect();
        assert_eq!(
            export_page(&source_bytes, &redone_patches, Target::SameAsSource)
                .unwrap()
                .bytes,
            exported.bytes
        );
        let (original_plan, _) = redone
            .load_text_shape_plan(&applied.region_id)
            .unwrap()
            .unwrap();
        let prepare_revision = |padding_px| {
            prepare_component_at(
                &library,
                true,
                analysis_id,
                &chapter.id,
                0,
                "sam-00001",
                true,
                padding_px,
                original_plan.additions.clone(),
                original_plan.removals.clone(),
                original_plan.correction_revision,
            )
            .unwrap()
        };
        let two = prepare_revision(2);
        apply_component_at(&library, true, &two.plan_id, &two.support_sha256).unwrap();
        let five = prepare_revision(5);
        apply_component_at(&library, true, &five.plan_id, &five.support_sha256).unwrap();
        let two_again = prepare_revision(2);
        apply_component_at(
            &library,
            true,
            &two_again.plan_id,
            &two_again.support_sha256,
        )
        .unwrap();
        assert_eq!(two.support_sha256, two_again.support_sha256);
        assert_ne!(two.plan_identity_sha256, two_again.plan_identity_sha256);
        let mut revised = Job::open(&path).unwrap();
        assert_eq!(
            revised
                .project
                .text_shape_plans
                .iter()
                .filter(|plan| plan.region_id == applied.region_id)
                .count(),
            4
        );
        assert_eq!(
            revised
                .project
                .patches
                .iter()
                .filter(|p| p.id == applied.region_id)
                .count(),
            1
        );
        assert!(
            revised
                .restore_text_shape_revision(&applied.region_id, &two.plan_identity_sha256)
                .unwrap()
        );
        let restored_two = revised
            .project
            .patches
            .iter()
            .find(|record| record.id == applied.region_id)
            .unwrap();
        assert_eq!(
            restored_two.text_shape_plan_identity.as_deref(),
            Some(two.plan_identity_sha256.as_str())
        );
        assert!(
            revised
                .restore_text_shape_revision(&applied.region_id, &two_again.plan_identity_sha256)
                .unwrap()
        );
        let restored_latest = revised
            .project
            .patches
            .iter()
            .find(|record| record.id == applied.region_id)
            .unwrap();
        assert_eq!(
            restored_latest.text_shape_plan_identity.as_deref(),
            Some(two_again.plan_identity_sha256.as_str())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A remote analysis stores itself while holding its chapter
    /// (`confirm_and_run`), so a component command must not hold the review
    /// store while it waits for that chapter: that was a deadlock. It waits
    /// with the store let go, and goes on once the chapter is free.
    #[test]
    fn a_component_preview_waiting_for_its_chapter_does_not_hold_the_review_store() {
        let _store = store_serial();
        use cleaner_core::image::{encode, fixtures, Format};
        let root = std::env::temp_dir().join(format!("mc-component-lock-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let scans = root.join("scans");
        std::fs::create_dir_all(&scans).unwrap();
        let mut source = fixtures::by_name("l8").raster;
        source.set_sample(10, 10, 0, 0);
        let bytes = encode(&source, Format::Png).unwrap();
        std::fs::write(scans.join("001.png"), &bytes).unwrap();
        let library = Library::at(root.join("library"));
        let project = library.create_project("order", StripMode::Single, Some(scans.clone()), None).unwrap();
        let chapter = library.create_chapter(&project.id, "chapter", None, Some(scans)).unwrap().created().unwrap();
        let path = library.resolve_chapter(&chapter.id).unwrap();
        let mut mask = vec![0; source.width as usize * source.height as usize];
        mask[10 * source.width as usize + 10] = 255;
        let analysis_id = "lock-order".to_owned();
        {
            let mut store = review_store().lock().unwrap();
            store.analysis = Some(StoredAnalysis {
                id: analysis_id.clone(), sam_backend: "ort-webgpu", write_eligible: true,
                runtime_sha256: Some(SAM_WEBGPU_RUNTIME_SHA256),
                source_sha256: format!("{:x}", Sha256::digest(&bytes)),
                model_sha256: "fixture-model".into(), width: source.width, height: source.height,
                mask, bubble_associated_components: std::collections::HashSet::new(),
                candidate_bounds: HashMap::new(),
            });
            store.prepared = None;
        }

        // The remote analysis's half: the chapter first.
        let chapter_held = crate::run::lock_job(&path).unwrap();
        let (store_free, prepared) = std::thread::scope(|scope| {
            let preview = scope.spawn(|| prepare_component_at(
                &library, true, &analysis_id, &chapter.id, 0, "sam-00001", true,
                0, empty_correction(), empty_correction(), 0,
            ));
            std::thread::sleep(std::time::Duration::from_millis(100));
            // Then the store, as `store_remote_analysis` takes it.
            let store_free = (0..40).any(|_| {
                let free = review_store().try_lock().is_ok();
                if !free {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                free
            });
            drop(chapter_held);
            (store_free, preview.join().unwrap())
        });
        assert!(store_free, "the preview held the review store while it waited for the chapter");
        prepared.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
