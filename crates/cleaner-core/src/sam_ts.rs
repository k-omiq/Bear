//! Mask-only SAM-TS-L inference: `mayocream/koharu-text-sam-ts-l`, the
//! separately published lettering-mask checkpoint, not Koharu's layout
//! detector. The two ONNX graphs are the pinned high-resolution lettering
//! path, not a generic SAM prompt decoder. This module has no detector, OCR,
//! language-gate, or cleaning dependency.
//! Input is the application's decoded `Raster`. JPEG decoder differences must
//! be resolved at ingest; this module does not decode an original JPEG again.
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::image::Raster;

pub const SIDE: usize = 1024;
pub const ENCODER: &str = "koharu_samts_encoder.onnx";
pub const HEAD: &str = "koharu_samts_text_head.onnx";

#[derive(Default)]
pub struct Cancellation {
    cancelled: AtomicBool,
    finished: AtomicBool,
    run_options: Mutex<Option<Arc<ort::session::RunOptions>>>,
}

impl Cancellation {
    pub fn cancel(&self) -> bool {
        if self.finished.load(Ordering::Acquire) { return false; }
        self.cancelled.store(true, Ordering::Release);
        if let Some(options) = self.run_options.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = options.terminate();
        }
        true
    }

    pub fn finish(&self) {
        self.finished.store(true, Ordering::Release);
    }

    pub fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            Err("analysis cancelled".into())
        } else {
            Ok(())
        }
    }

    pub(crate) fn options(&self) -> Result<Arc<ort::session::RunOptions>, String> {
        self.check()?;
        let mut slot = self.run_options.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(Arc::new(ort::session::RunOptions::new().map_err(|e| e.to_string())?));
        }
        let options = slot.as_ref().unwrap().clone();
        if self.cancelled.load(Ordering::Acquire) {
            let _ = options.terminate();
            return Err("analysis cancelled".into());
        }
        Ok(options)
    }
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub chw: Vec<f32>,
    pub resized_width: usize,
    pub resized_height: usize,
    pub original_width: usize,
    pub original_height: usize,
}

/// Pillow's `round(axis * 1024 / max_axis)` uses ties-to-even.
fn resized_axis(axis: usize, longest: usize) -> usize {
    let numerator = axis * SIDE;
    let whole = numerator / longest;
    let remainder = numerator % longest;
    (whole + usize::from(remainder * 2 > longest || (remainder * 2 == longest && whole % 2 != 0)))
        .max(1)
}

/// Prepare RGB bytes, bilinear resize, and a top-left gray-128 pad. The graph
/// itself performs the pinned mean/std normalization exactly once.
pub fn prepare(page: &Raster) -> Result<Prepared, String> {
    let width = page.width as usize;
    let height = page.height as usize;
    if width == 0 || height == 0 || width.checked_mul(height).is_none() {
        return Err("SAM-TS-L requires a nonempty bounded page".into());
    }
    let longest = width.max(height);
    let resized_width = resized_axis(width, longest);
    let resized_height = resized_axis(height, longest);
    let input = page.to_rgb8();
    let resized = pillow_bilinear_rgb(&input, width, height, resized_width, resized_height);
    let mut chw = vec![128f32; 3 * SIDE * SIDE];
    for y in 0..resized_height {
        for x in 0..resized_width {
            for c in 0..3 {
                chw[c * SIDE * SIDE + y * SIDE + x] =
                    resized[(y * resized_width + x) * 3 + c] as f32;
            }
        }
    }
    Ok(Prepared {
        chw,
        resized_width,
        resized_height,
        original_width: width,
        original_height: height,
    })
}

/// Pillow's RGB bilinear convolution performs horizontal and vertical passes
/// with an 8-bit intermediate. Coefficients are normalized per destination
/// sample and quantized to 22 fixed-point fractional bits, matching its
/// `Resample.c` uint8 path. The support expands when reducing an image.
fn coefficients(input: usize, output: usize) -> Vec<Vec<(usize, i64)>> {
    let scale = input as f64 / output as f64;
    let filter_scale = scale.max(1.0);
    let support = filter_scale;
    (0..output)
        .map(|destination| {
            let center = (destination as f64 + 0.5) * scale;
            let left = ((center - support + 0.5).floor() as isize).max(0) as usize;
            let right = ((center + support + 0.5).floor() as isize).min(input as isize) as usize;
            let mut taps: Vec<(usize, f64)> = (left..right)
                .map(|source| {
                    let distance = ((source as f64 + 0.5 - center) / filter_scale).abs();
                    (source, (1.0 - distance).max(0.0))
                })
                .collect();
            let total: f64 = taps.iter().map(|(_, weight)| weight).sum();
            for (_, weight) in &mut taps {
                *weight /= total;
            }
            taps.into_iter()
                .map(|(source, weight)| (source, (weight * (1 << 22) as f64).round() as i64))
                .collect()
        })
        .collect()
}

pub(crate) fn pillow_bilinear_rgb(
    input: &[u8],
    width: usize,
    height: usize,
    out_width: usize,
    out_height: usize,
) -> Vec<u8> {
    let x_weights = coefficients(width, out_width);
    let y_weights = coefficients(height, out_height);
    let mut horizontal = vec![0u8; out_width * height * 3];
    for y in 0..height {
        for (x, taps) in x_weights.iter().enumerate() {
            for c in 0..3 {
                let sum = taps.iter().fold(1i64 << 21, |acc, (source, weight)| {
                    acc + input[(y * width + source) * 3 + c] as i64 * weight
                });
                horizontal[(y * out_width + x) * 3 + c] = (sum >> 22).clamp(0, 255) as u8;
            }
        }
    }
    let mut output = vec![0u8; out_width * out_height * 3];
    for (y, taps) in y_weights.iter().enumerate() {
        for x in 0..out_width {
            for c in 0..3 {
                let sum = taps.iter().fold(1i64 << 21, |acc, (source, weight)| {
                    acc + horizontal[(source * out_width + x) * 3 + c] as i64 * weight
                });
                output[(y * out_width + x) * 3 + c] = (sum >> 22).clamp(0, 255) as u8;
            }
        }
    }
    output
}

/// Threshold *before* crop and nearest-neighbor restoration, as in the
/// reference inference.py. Output is an original-page 0/255 raster.
pub fn restore(logits: &[f32], prepared: &Prepared) -> Result<Vec<u8>, String> {
    if logits.len() != SIDE * SIDE || logits.iter().any(|v| !v.is_finite()) {
        return Err("SAM-TS-L head returned an invalid 1024 square logits tensor".into());
    }
    // Pillow 11.3.0's nearest resize precomputes x indexes and advances the
    // y coordinate by repeated floating-point addition. Integer center
    // arithmetic differs at exact half-pixel boundaries (1024 -> 1600 is one
    // such case), even though it looks mathematically equivalent.
    fn pillow_nearest_indexes(input: usize, output: usize) -> Vec<usize> {
        let step = input as f64 / output as f64;
        let mut source = step * 0.5;
        (0..output)
            .map(|_| {
                let index = (source as usize).min(input - 1);
                source += step;
                index
            })
            .collect()
    }
    let xs = pillow_nearest_indexes(prepared.resized_width, prepared.original_width);
    let ys = pillow_nearest_indexes(prepared.resized_height, prepared.original_height);
    let mut mask = vec![0; prepared.original_width * prepared.original_height];
    for y in 0..prepared.original_height {
        let sy = ys[y];
        for x in 0..prepared.original_width {
            let sx = xs[x];
            mask[y * prepared.original_width + x] = u8::from(logits[sy * SIDE + sx] > 0.0) * 255;
        }
    }
    Ok(mask)
}

#[derive(Debug)]
pub struct Inference {
    pub mask: Vec<u8>,
    pub load: Duration,
    pub prepare: Duration,
    pub encoder: Duration,
    pub head: Duration,
    pub restore: Duration,
    /// Present only for an explicit WebGPU run whose two profiles were checked.
    pub webgpu_nodes: Option<(u64, u64)>,
    /// A successful explicit WebGPU run must have zero CPU node events.
    pub cpu_fallback_nodes: Option<(u64, u64)>,
    /// Process high-water RSS; cumulative within this process, not GPU memory.
    pub process_high_water_bytes: Option<u64>,
    /// Current macOS task physical footprint, including compressed and GPU-backed memory.
    pub process_phys_footprint_bytes: Option<u64>,
    /// `MTLDevice.currentAllocatedSize` polled every 20 ms; may miss a spike.
    pub sampled_metal_high_water_bytes: Option<u64>,
}

/// Explicit CPU path. A requested GPU must go through a separately qualified
/// implementation; ORT provider presence alone is not proof of GPU execution.
pub fn infer_cpu(page: &Raster, graph_dir: &Path) -> Result<Inference, String> {
    infer_cpu_cancellable(page, graph_dir, &Cancellation::default())
}

pub fn infer_cpu_cancellable(
    page: &Raster,
    graph_dir: &Path,
    cancel: &Cancellation,
) -> Result<Inference, String> {
    SamTsSession::open_cancellable(graph_dir, cancel)?.infer(page, cancel)
}

/// One encoder/head pair for a run. The lease lives with both ORT sessions,
/// so the loaded-models row disappears as soon as the owner drops this value.
/// Pinned graph SHA-256 checks belong to the caller's installation boundary;
/// a run should perform those once before opening this session.
pub struct SamTsSession {
    encoder: ort::session::Session,
    head: ort::session::Session,
    first_load: Option<Duration>,
    lease: crate::registry::Lease,
    backend: crate::accel::Accelerator,
    profiles: Option<ProfileCleanup>,
    verified_nodes: Option<(u64, u64)>,
}

impl SamTsSession {
    pub fn open(graph_dir: &Path) -> Result<Self, String> {
        Self::open_cancellable(graph_dir, &Cancellation::default())
    }

    pub fn open_cancellable(graph_dir: &Path, cancel: &Cancellation) -> Result<Self, String> {
        Self::open_on(graph_dir, crate::accel::Accelerator::Cpu, cancel)
    }

    pub fn open_on(graph_dir: &Path, backend: crate::accel::Accelerator, cancel: &Cancellation) -> Result<Self, String> {
        cancel.check()?;
        let started = Instant::now();
        if !matches!(backend, crate::accel::Accelerator::Cpu | crate::accel::Accelerator::WebGpu) {
            return Err(format!("SAM-TS-L does not support {backend:?}"));
        }
        if backend == crate::accel::Accelerator::WebGpu
            && (!cfg!(target_os = "macos") || !built_in_webgpu_available()) {
            return Err("SAM-TS-L WebGPU needs the built-in Apple WebGPU runtime and device".into());
        }
        let profiles = (backend == crate::accel::Accelerator::WebGpu)
            .then(|| ProfileCleanup([profile_prefix("encoder"), profile_prefix("head")]));
        let open = |path: &Path, index: usize| -> Result<ort::session::Session, String> {
            if let Some(profiles) = profiles.as_ref() {
                webgpu_session(path, &profiles.0[index])
            } else {
                ort::session::Session::builder().map_err(|e| e.to_string())?
                    .with_intra_threads(4).map_err(|e| e.to_string())?
                    .commit_from_file(path).map_err(|e| e.to_string())
            }
        };
        let encoder = open(&graph_dir.join(ENCODER), 0)
            .map_err(|e| format!("SAM-TS-L encoder: {e}"))?;
        cancel.check()?;
        let head = open(&graph_dir.join(HEAD), 1)
            .map_err(|e| format!("SAM-TS-L text head: {e}"))?;
        cancel.check()?;
        let first_load = Some(started.elapsed());
        let lease = crate::registry::register_named(
            crate::registry::Kind::TextDetector,
            crate::registry::Footprint::weights_of(&[
                &graph_dir.join(ENCODER),
                &graph_dir.join(HEAD),
            ]),
            crate::registry::Device::accelerator(backend),
            Some("SAM-TS-L lettering mask".into()),
        );
        Ok(Self {
            encoder,
            head,
            first_load,
            lease,
            backend,
            profiles,
            verified_nodes: None,
        })
    }

    /// Honor unload requests at a segment boundary. The caller owns that safe
    /// point and may drop this value, then reopen it if a later segment needs it.
    pub fn spent(&self) -> bool {
        self.lease.spent()
    }

    pub fn backend(&self) -> crate::accel::Accelerator { self.backend }

    pub fn infer(&mut self, page: &Raster, cancel: &Cancellation) -> Result<Inference, String> {
        cancel.check()?;
        self.lease.touch();
        let started = Instant::now();
        let prepared = prepare(page)?;
        let prepare_time = started.elapsed();
        cancel.check()?;
        let options = cancel.options()?;
        let input = ort::value::Tensor::from_array(([1usize, 3, SIDE, SIDE], prepared.chw.clone()))
            .map_err(|e| e.to_string())?;
        let started = Instant::now();
        let embedding = self
            .encoder
            .run_with_options(ort::inputs!["prepared_rgb" => input], &options)
            .map_err(|e| {
                if cancel.check().is_err() {
                    "analysis cancelled".into()
                } else {
                    format!("SAM-TS-L encoder inference: {e}")
                }
            })?;
        cancel.check()?;
        let (_, values) = embedding["embedding"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("SAM-TS-L embedding: {e}"))?;
        if values.len() != 256 * 64 * 64 || values.iter().any(|v| !v.is_finite()) {
            return Err("SAM-TS-L encoder output shape or values are invalid".into());
        }
        let embedding_values = values.to_vec();
        let encoder_time = started.elapsed();
        drop(embedding);
        cancel.check()?;
        let input = ort::value::Tensor::from_array(([1usize, 256, 64, 64], embedding_values))
            .map_err(|e| e.to_string())?;
        let started = Instant::now();
        let output = self
            .head
            .run_with_options(ort::inputs!["embedding" => input], &options)
            .map_err(|e| {
                if cancel.check().is_err() {
                    "analysis cancelled".into()
                } else {
                    format!("SAM-TS-L text-head inference: {e}")
                }
            })?;
        cancel.check()?;
        let (_, logits) = output["high_res_logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("SAM-TS-L logits: {e}"))?;
        let head_time = started.elapsed();
        let started = Instant::now();
        let mask = restore(logits, &prepared)?;
        cancel.check()?;
        let restore_time = started.elapsed();
        drop(output);
        if self.backend == crate::accel::Accelerator::WebGpu && self.verified_nodes.is_none() {
            let verified = (|| {
                let encoder = verified_webgpu_nodes(&mut self.encoder, "encoder")?;
                let head = verified_webgpu_nodes(&mut self.head, "head")?;
                Ok::<_, String>((encoder, head))
            })();
            self.profiles = None;
            match verified {
                Ok(nodes) => self.verified_nodes = Some(nodes),
                Err(error) => { self.lease.poison(); return Err(error); }
            }
        }
        Ok(Inference {
            mask,
            load: self.first_load.take().unwrap_or_default(),
            prepare: prepare_time,
            encoder: encoder_time,
            head: head_time,
            restore: restore_time,
            webgpu_nodes: self.verified_nodes,
            cpu_fallback_nodes: self.verified_nodes.map(|_| (0, 0)),
            process_high_water_bytes: process_high_water_bytes(),
            process_phys_footprint_bytes: process_phys_footprint_bytes(),
            sampled_metal_high_water_bytes: None,
        })
    }
}

/// Whether the loaded runtime exposes a built-in WebGPU EP with a device.
/// The application uses this only on macOS; plugin-only runtimes are not
/// qualified by the app's Apple-provider measurements.
pub fn built_in_webgpu_available() -> bool {
    use ort::ep::ExecutionProvider;
    ort::ep::webgpu::WebGPU::default()
        .is_available()
        .unwrap_or(false)
        && ort::environment::Environment::current().is_ok_and(|env| {
            env.devices().any(|device| {
                device.ep().ok() == Some("WebGpuExecutionProvider")
                    && device.hardware_device().ty() == ort::memory::DeviceType::GPU
                    && device.hardware_device().vendor().ok() == Some("Apple")
            })
        })
}

/// The measured Mac class: MacBook Air Mac17,3, M5 Metal GPU, 32 GB RAM.
/// Other Apple GPUs can use explicit review, but do not inherit this host's
/// exact-mask and memory qualification for supervised writing.
#[cfg(target_os = "macos")]
pub fn measured_m5_host() -> bool {
    static MEASURED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MEASURED.get_or_init(measured_m5_host_uncached)
}
#[cfg(target_os = "macos")]
fn measured_m5_host_uncached() -> bool {
    use std::ffi::{CStr, c_char, c_void};
    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> *mut c_void;
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend(receiver: *mut c_void, selector: *mut c_void) -> usize;
    }
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const u8,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *const c_void,
            newlen: usize,
        ) -> i32;
    }
    unsafe fn sysctl_string(name: &CStr) -> Option<String> {
        let mut len = 0;
        if unsafe {
            sysctlbyname(
                name.as_ptr().cast(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null(),
                0,
            )
        } != 0
        {
            return None;
        }
        let mut value = vec![0u8; len];
        if unsafe {
            sysctlbyname(
                name.as_ptr().cast(),
                value.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null(),
                0,
            )
        } != 0
        {
            return None;
        }
        Some(
            String::from_utf8_lossy(&value[..len])
                .trim_end_matches('\0')
                .to_owned(),
        )
    }
    unsafe fn sysctl_u64(name: &CStr) -> Option<u64> {
        let mut value = 0u64;
        let mut len = std::mem::size_of::<u64>();
        (unsafe {
            sysctlbyname(
                name.as_ptr().cast(),
                (&mut value as *mut u64).cast(),
                &mut len,
                std::ptr::null(),
                0,
            )
        } == 0
            && len == 8)
            .then_some(value)
    }
    let metal_name = unsafe {
        let device = MTLCreateSystemDefaultDevice();
        if device.is_null() {
            return false;
        }
        let object = objc_msgSend(device, sel_registerName(c"name".as_ptr())) as *mut c_void;
        let name = if object.is_null() {
            None
        } else {
            let utf8 =
                objc_msgSend(object, sel_registerName(c"UTF8String".as_ptr())) as *const c_char;
            (!utf8.is_null()).then(|| CStr::from_ptr(utf8).to_string_lossy().into_owned())
        };
        let _ = objc_msgSend(device, sel_registerName(c"release".as_ptr()));
        name
    };
    metal_name.as_deref() == Some("Apple M5")
        && unsafe { sysctl_string(c"machdep.cpu.brand_string") }.as_deref() == Some("Apple M5")
        && unsafe { sysctl_string(c"hw.model") }.as_deref() == Some("Mac17,3")
        && unsafe { sysctl_u64(c"hw.memsize") } == Some(34_359_738_368)
        && unsafe { sysctl_string(c"kern.osrelease") }.as_deref() == Some("27.0.0")
        && std::process::Command::new("/usr/sbin/system_profiler")
            .args(["SPDisplaysDataType", "-json"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<serde_json::Value>(&output.stdout).ok())
            .and_then(|value| value["SPDisplaysDataType"].as_array().cloned())
            .is_some_and(|devices| {
                devices.iter().any(|device| {
                    device["_name"] == "Apple M5"
                        && device["sppci_cores"] == "10"
                        && device["spdisplays_mtlgpufamilysupport"] == "spdisplays_metal4"
                })
            })
}
#[cfg(not(target_os = "macos"))]
pub fn measured_m5_host() -> bool {
    false
}

fn webgpu_session(model: &Path, profile: &Path) -> Result<ort::session::Session, String> {
    let env = ort::environment::Environment::current().map_err(|e| e.to_string())?;
    let devices: Vec<_> = env
        .devices()
        .filter(|device| device.ep().ok() == Some("WebGpuExecutionProvider"))
        .collect();
    if devices.is_empty() {
        return Err("SAM-TS-L WebGPU was requested but no WebGPU EP device was discovered".into());
    }
    // Do not use accel::open_session here: its generic policy retries on CPU.
    // ORT itself also has CPU fallback unless explicitly disabled.
    let mut builder = ort::session::Session::builder()
        .map_err(|e| e.to_string())?
        .with_intra_threads(4)
        .map_err(|e| e.to_string())?
        .with_disable_cpu_fallback()
        .map_err(|e| e.to_string())?
        .with_profiling(profile)
        .map_err(|e| e.to_string())?
        .with_devices(devices, None)
        .map_err(|e| e.to_string())?;
    builder
        .commit_from_file(model)
        .map_err(|e| format!("SAM-TS-L WebGPU session {}: {e}", model.display()))
}

fn verified_webgpu_nodes(session: &mut ort::session::Session, graph: &str) -> Result<u64, String> {
    let path = PathBuf::from(session.end_profiling().map_err(|e| e.to_string())?);
    let result = (|| {
        let events: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let events = events
            .as_array()
            .ok_or("ORT profile was not an event array")?;
        let mut webgpu = 0;
        for event in events {
            if event.get("cat").and_then(serde_json::Value::as_str) != Some("Node") {
                continue;
            }
            let provider = event
                .pointer("/args/provider")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("SAM-TS-L {graph} profile omitted a node provider"))?;
            if provider != "WebGpuExecutionProvider" {
                return Err(format!(
                    "SAM-TS-L {graph} ran a node on {provider}; WebGPU requested without CPU fallback"
                ));
            }
            webgpu += 1;
        }
        if webgpu == 0 {
            return Err(format!("SAM-TS-L {graph} profile has no WebGPU nodes"));
        }
        Ok(webgpu)
    })();
    let _ = std::fs::remove_file(path);
    result
}

fn profile_prefix(graph: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "manga-cleaner-samts-{graph}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// ORT appends a timestamp to a profile prefix. Remove even a partial file
/// after an inference error, when `end_profiling` was never reached.
struct ProfileCleanup([PathBuf; 2]);
impl Drop for ProfileCleanup {
    fn drop(&mut self) {
        for prefix in &self.0 {
            let Some(parent) = prefix.parent() else {
                continue;
            };
            let Some(stem) = prefix.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    if entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&format!("{stem}_"))
                    {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn metal_current_allocated_bytes() -> Option<u64> {
    use std::ffi::{c_char, c_void};
    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> *mut c_void;
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend(receiver: *mut c_void, selector: *mut c_void) -> usize;
    }
    unsafe {
        let device = MTLCreateSystemDefaultDevice();
        if device.is_null() {
            return None;
        }
        let bytes = objc_msgSend(device, sel_registerName(c"currentAllocatedSize".as_ptr())) as u64;
        let _ = objc_msgSend(device, sel_registerName(c"release".as_ptr()));
        Some(bytes)
    }
}
#[cfg(not(target_os = "macos"))]
fn metal_current_allocated_bytes() -> Option<u64> {
    None
}

struct MetalSampler {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Option<u64>>>,
}
impl MetalSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut maximum: Option<u64> = None;
            while !thread_stop.load(Ordering::Relaxed) {
                if let Some(bytes) = metal_current_allocated_bytes() {
                    maximum = Some(maximum.map_or(bytes, |old| old.max(bytes)));
                }
                thread::sleep(Duration::from_millis(20));
            }
            maximum
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
    fn finish(mut self) -> Option<u64> {
        self.stop_join()
    }
    fn stop_join(&mut self) -> Option<u64> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .and_then(|thread| thread.join().ok().flatten())
    }
}
impl Drop for MetalSampler {
    fn drop(&mut self) {
        let _ = self.stop_join();
    }
}

#[cfg(target_os = "macos")]
fn process_high_water_bytes() -> Option<u64> {
    #[repr(C)]
    struct TimeVal {
        sec: i64,
        usec: i32,
    }
    #[repr(C)]
    struct Usage {
        utime: TimeVal,
        stime: TimeVal,
        maxrss: i64,
        rest: [u8; 240],
    }
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Usage) -> i32;
    }
    let mut usage = std::mem::MaybeUninit::<Usage>::zeroed();
    unsafe { (getrusage(0, usage.as_mut_ptr()) == 0).then(|| usage.assume_init().maxrss as u64) }
}
#[cfg(not(target_os = "macos"))]
fn process_high_water_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn process_phys_footprint_bytes() -> Option<u64> {
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn task_info(task: u32, flavor: u32, info: *mut u32, count: *mut u32) -> i32;
    }
    let mut info = [0u32; 128];
    let mut count = info.len() as u32;
    // TASK_VM_INFO is flavor 22. phys_footprint follows 16 u64 fields and
    // the virtual-size / region-count / page-size header in packed(4) layout.
    let status = unsafe { task_info(mach_task_self_, 22, info.as_mut_ptr(), &mut count) };
    (status == 0 && count >= 38).then(|| {
        let lo = info[36].to_ne_bytes();
        let hi = info[37].to_ne_bytes();
        u64::from_ne_bytes([lo[0], lo[1], lo[2], lo[3], hi[0], hi[1], hi[2], hi[3]])
    })
}
#[cfg(not(target_os = "macos"))]
fn process_phys_footprint_bytes() -> Option<u64> {
    None
}

/// Explicit Apple WebGPU review path. The installed runtime must expose a
/// WebGPU device, both graphs must commit without CPU fallback, and runtime
/// profiles must show every executed node on WebGPU. Any violation is an error.
pub fn infer_webgpu(page: &Raster, graph_dir: &Path) -> Result<Inference, String> {
    infer_webgpu_cancellable(page, graph_dir, &Cancellation::default())
}

pub fn infer_webgpu_cancellable(page: &Raster, graph_dir: &Path, cancel: &Cancellation) -> Result<Inference, String> {
    cancel.check()?;
    if !built_in_webgpu_available() {
        return Err("SAM-TS-L WebGPU is absent from the installed runtime or has no device".into());
    }
    let sampler = MetalSampler::start();
    let profiles = ProfileCleanup([profile_prefix("encoder"), profile_prefix("head")]);
    let started = Instant::now();
    let mut encoder = webgpu_session(&graph_dir.join(ENCODER), &profiles.0[0])?;
    let mut head = webgpu_session(&graph_dir.join(HEAD), &profiles.0[1])?;
    let load = started.elapsed();
    let started = Instant::now();
    let prepared = prepare(page)?;
    let prepare_time = started.elapsed();
    cancel.check()?;
    let options = cancel.options()?;
    let input = ort::value::Tensor::from_array(([1usize, 3, SIDE, SIDE], prepared.chw.clone()))
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let embedding = encoder
        .run_with_options(ort::inputs!["prepared_rgb" => input], &options)
        .map_err(|e| if cancel.check().is_err() { "analysis cancelled".into() } else { format!("SAM-TS-L WebGPU encoder inference: {e}") })?;
    cancel.check()?;
    let (_, values) = embedding["embedding"]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("SAM-TS-L WebGPU embedding: {e}"))?;
    if values.len() != 256 * 64 * 64 || values.iter().any(|v| !v.is_finite()) {
        return Err("SAM-TS-L WebGPU encoder output shape or values are invalid".into());
    }
    let embedding_values = values.to_vec();
    let encoder_time = started.elapsed();
    drop(embedding);
    let input = ort::value::Tensor::from_array(([1usize, 256, 64, 64], embedding_values))
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let output = head
        .run_with_options(ort::inputs!["embedding" => input], &options)
        .map_err(|e| if cancel.check().is_err() { "analysis cancelled".into() } else { format!("SAM-TS-L WebGPU text-head inference: {e}") })?;
    cancel.check()?;
    let (_, logits) = output["high_res_logits"]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("SAM-TS-L WebGPU logits: {e}"))?;
    let head_time = started.elapsed();
    let started = Instant::now();
    let mask = restore(logits, &prepared)?;
    cancel.check()?;
    let restore_time = started.elapsed();
    drop(output);
    let encoder_nodes = verified_webgpu_nodes(&mut encoder, "encoder")?;
    let head_nodes = verified_webgpu_nodes(&mut head, "head")?;
    let process_high_water_bytes = process_high_water_bytes();
    let process_phys_footprint_bytes = process_phys_footprint_bytes();
    let sampled_metal_high_water_bytes = sampler.finish();
    Ok(Inference {
        mask,
        load,
        prepare: prepare_time,
        encoder: encoder_time,
        head: head_time,
        restore: restore_time,
        webgpu_nodes: Some((encoder_nodes, head_nodes)),
        cpu_fallback_nodes: Some((0, 0)),
        process_high_water_bytes,
        process_phys_footprint_bytes,
        sampled_metal_high_water_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_checked_before_a_graph_is_loaded() {
        let cancelled = Cancellation::default();
        cancelled.cancel();
        assert_eq!(cancelled.check().unwrap_err(), "analysis cancelled");
        let page = crate::image::fixtures::by_name("rgb8").raster;
        assert_eq!(infer_cpu_cancellable(&page, Path::new("/absent"), &cancelled).unwrap_err(), "analysis cancelled");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_task_footprint_is_measured() {
        assert!(process_phys_footprint_bytes().is_some_and(|bytes| bytes > 0));
    }

    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    fn npy_payload(bytes: &[u8]) -> &[u8] {
        assert_eq!(&bytes[..6], b"\x93NUMPY");
        let offset = if bytes[6] == 1 {
            10 + u16::from_le_bytes([bytes[8], bytes[9]]) as usize
        } else {
            12 + u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize
        };
        &bytes[offset..]
    }

    #[test]
    fn prepared_rgb_matches_saved_pytorch_digest() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l");
        let page =
            crate::image::decode(&std::fs::read(root.join("fixtures/synthetic_page.png")).unwrap())
                .unwrap();
        let actual = prepare(&page).unwrap();
        assert_eq!((actual.resized_width, actual.resized_height), (750, 1024));
        let mut digest = Sha256::new();
        for value in &actual.chw {
            digest.update(value.to_le_bytes());
        }
        assert_eq!(
            format!("{:x}", digest.finalize()),
            "4246c79109fb8f26bf3e37d4917f56a43ad9dcea87780ff2a18aae60ea9dbba4"
        );
    }

    #[test]
    #[ignore = "compares against local proof tensors under ignored artifacts/"]
    fn prepared_rgb_matches_saved_pytorch_input() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l");
        let page =
            crate::image::decode(&std::fs::read(root.join("fixtures/synthetic_page.png")).unwrap())
                .unwrap();
        let actual = prepare(&page).unwrap();
        let bytes = std::fs::read(root.join("artifacts/proof/prepared_rgb_f32.npy")).unwrap();
        // NumPy v1/v2 header, little-endian f32, C-order. The saved fixture is
        // a reference artifact, not output generated by this implementation.
        let payload = npy_payload(&bytes);
        assert_eq!(payload.len(), actual.chw.len() * 4);
        let changed = actual
            .chw
            .iter()
            .zip(payload.chunks_exact(4))
            .filter(|(a, b)| **a != f32::from_le_bytes((*b).try_into().unwrap()))
            .count();
        assert_eq!(
            changed, 0,
            "Pillow bilinear prepared-input parity failed at {changed} samples"
        );
    }

    #[test]
    #[ignore = "compares against local proof tensors under ignored artifacts/"]
    fn restored_mask_matches_saved_reference() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l/artifacts/proof");
        let bytes = std::fs::read(root.join("reference_logits_f32.npy")).unwrap();
        let logits: Vec<f32> = npy_payload(&bytes)
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let prepared = Prepared {
            chw: Vec::new(),
            resized_width: 750,
            resized_height: 1024,
            original_width: 701,
            original_height: 957,
        };
        let actual = restore(&logits, &prepared).unwrap();
        let expected =
            npy_payload(&std::fs::read(root.join("reference_restored_mask_u8.npy")).unwrap())
                .to_vec();
        assert_eq!(actual, expected);
    }

    #[test]
    fn nearest_restore_matches_pillow_half_pixel_boundary() {
        let mut logits = vec![-1.0; SIDE * SIDE];
        for x in 0..SIDE {
            logits[7 * SIDE + x] = 1.0;
        }
        let prepared = Prepared {
            chw: Vec::new(),
            resized_width: SIDE,
            resized_height: SIDE,
            original_width: SIDE,
            original_height: 1600,
        };
        let mask = restore(&logits, &prepared).unwrap();
        assert_eq!(mask[12 * SIDE], 255); // Pillow maps destination row 12 to source row 7.
        assert_eq!(mask[13 * SIDE], 0);
    }

    #[test]
    #[ignore = "requires saved page 07 parity tensors"]
    fn restored_page07_reference() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../spikes/sam-ts-l/artifacts/examples/apocalypse-109/page-07");
        let bytes = std::fs::read(root.join("onnx_head_logits_f32.npy")).unwrap();
        let logits: Vec<f32> = npy_payload(&bytes)
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let prepared = Prepared {
            chw: Vec::new(),
            resized_width: 442,
            resized_height: 1024,
            original_width: 690,
            original_height: 1600,
        };
        let actual = restore(&logits, &prepared).unwrap();
        let expected_bytes = std::fs::read(root.join("onnx_restored_mask_u8.npy")).unwrap();
        let expected = npy_payload(&expected_bytes);
        let changed = actual.iter().zip(expected).filter(|(a, b)| a != b).count();
        assert_eq!(
            changed, 0,
            "page 07 nearest restoration differs at {changed} pixels"
        );
    }

    #[test]
    #[ignore = "loads the 1.3 GB SAM graph; set SAM_TS_RUNTIME to a local ONNX Runtime library"]
    fn native_cpu_graph_matches_saved_mask() {
        let runtime = std::env::var("SAM_TS_RUNTIME").expect("SAM_TS_RUNTIME is required");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l");
        let page =
            crate::image::decode(&std::fs::read(root.join("fixtures/synthetic_page.png")).unwrap())
                .unwrap();
        let inferred = infer_cpu(&page, &root.join("artifacts/proof")).unwrap();
        let expected = npy_payload(
            &std::fs::read(root.join("artifacts/proof/onnx_restored_mask_u8.npy")).unwrap(),
        )
        .to_vec();
        assert_eq!(inferred.mask, expected);
        eprintln!(
            "native SAM-TS-L CPU load {:?}, encoder {:?}, head {:?}",
            inferred.load, inferred.encoder, inferred.head
        );
    }

    #[test]
    #[ignore = "requires the app macOS ONNX runtime and local SAM graphs"]
    fn native_webgpu_session_reuses_verified_assignment() {
        let runtime = std::env::var("SAM_TS_RUNTIME").expect("SAM_TS_RUNTIME");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l");
        let page = crate::image::decode(&std::fs::read(root.join("fixtures/synthetic_page.png")).unwrap()).unwrap();
        let mut session = SamTsSession::open_on(
            &root.join("artifacts/proof"), crate::accel::Accelerator::WebGpu, &Cancellation::default()
        ).unwrap();
        let first = session.infer(&page, &Cancellation::default()).unwrap();
        let second = session.infer(&page, &Cancellation::default()).unwrap();
        assert_eq!(first.mask, second.mask);
        assert_eq!(first.webgpu_nodes, second.webgpu_nodes);
        assert!(first.webgpu_nodes.is_some_and(|(encoder, head)| encoder > 0 && head > 0));
        assert_eq!(first.cpu_fallback_nodes, Some((0, 0)));
        assert_eq!(second.load, Duration::default());
    }

    #[test]
    #[ignore = "requires app macOS ORT dylib, Pillow-decoded page 07 PNG, and local SAM graphs"]
    fn native_webgpu_page07_has_full_assignment_and_exact_mask() {
        let runtime = std::env::var("SAM_TS_RUNTIME").expect("SAM_TS_RUNTIME is required");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let source = std::env::var("SAM_TS_DECODED_PNG").expect("SAM_TS_DECODED_PNG is required");
        let page = crate::image::decode(&std::fs::read(source).unwrap()).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l");
        let result = infer_webgpu(&page, &root.join("artifacts/proof")).unwrap();
        let expected =
            npy_payload(
                &std::fs::read(root.join(
                    "artifacts/examples/apocalypse-109/page-07/reference_restored_mask_u8.npy",
                ))
                .unwrap(),
            )
            .to_vec();
        assert_eq!(result.mask, expected);
        assert_eq!(result.webgpu_nodes, Some((1818, 386)));
        assert!(
            result
                .sampled_metal_high_water_bytes
                .is_some_and(|bytes| bytes > 0)
        );
        eprintln!(
            "SAM-TS-L WebGPU: load {:?}, encoder {:?}, head {:?}, RSS {:?}, sampled Metal {:?}",
            result.load,
            result.encoder,
            result.head,
            result.process_high_water_bytes,
            result.sampled_metal_high_water_bytes
        );
    }

    #[test]
    #[ignore = "requires the measured M5, app ORT runtime, and local SAM graphs; reports a 4000x6000 synthetic page resource sample"]
    fn native_webgpu_4000x6000_resource_sample() {
        let runtime = std::env::var("SAM_TS_RUNTIME").expect("SAM_TS_RUNTIME is required");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let mut page = crate::image::fixtures::by_name("l8").raster;
        page.width = 4000;
        page.height = 6000;
        page.data = vec![255; page.width as usize * page.height as usize];
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l");
        let started = Instant::now();
        let result = infer_webgpu(&page, &root.join("artifacts/proof")).unwrap();
        assert_eq!(
            result.mask.len(),
            page.width as usize * page.height as usize
        );
        assert_eq!(result.cpu_fallback_nodes, Some((0, 0)));
        assert!(
            result
                .webgpu_nodes
                .is_some_and(|(encoder, head)| encoder > 0 && head > 0)
        );
        eprintln!(
            "4000x6000 SAM-TS-L WebGPU synthetic: total {:?}, load {:?}, prep {:?}, encoder {:?}, head {:?}, restore {:?}, RSS {:?}, sampled Metal {:?}",
            started.elapsed(),
            result.load,
            result.prepare,
            result.encoder,
            result.head,
            result.restore,
            result.process_high_water_bytes,
            result.sampled_metal_high_water_bytes
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "asserts the measured Mac17,3 M5 hardware identity"]
    fn measured_m5_host_identity() {
        assert!(measured_m5_host());
    }

    #[test]
    #[ignore = "requires page 07 JPEG under SAM_TS_CHAPTER_DIR"]
    fn native_page07_prepared_input_parity() {
        let sources = std::env::var("SAM_TS_CHAPTER_DIR").expect("SAM_TS_CHAPTER_DIR is required");
        let decoded = std::env::var("SAM_TS_DECODED_PNG").ok();
        let source_path = decoded
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(&sources).join("07.jpg"));
        let source = std::fs::read(source_path).unwrap();
        let page = if decoded.is_some() {
            crate::image::decode(&source).unwrap()
        } else {
            crate::image::foreign::decode(&source).unwrap()
        };
        let prepared = prepare(&page).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../spikes/sam-ts-l/artifacts/examples/apocalypse-109/page-07");
        let bytes = std::fs::read(root.join("prepared_rgb_f32.npy")).unwrap();
        let expected = npy_payload(&bytes);
        let changed = prepared
            .chw
            .iter()
            .zip(expected.chunks_exact(4))
            .filter(|(a, b)| **a != f32::from_le_bytes((*b).try_into().unwrap()))
            .count();
        assert_eq!(
            changed, 0,
            "native JPEG/Pillow prepared RGB differs at {changed} channel samples"
        );
    }

    #[test]
    #[ignore = "requires page 27 JPEG and its Pillow-decoded RGB PNG"]
    fn diagnose_page27_jpeg_decode() {
        let sources = std::env::var("SAM_TS_CHAPTER_DIR").expect("SAM_TS_CHAPTER_DIR is required");
        let png = std::env::var("SAM_TS_DECODED_PNG").expect("SAM_TS_DECODED_PNG is required");
        let jpeg = crate::image::foreign::decode(
            &std::fs::read(Path::new(&sources).join("27.jpg")).unwrap(),
        )
        .unwrap();
        if let Ok(path) = std::env::var("SAM_TS_NATIVE_PNG") {
            let png = crate::image::encode(&jpeg, crate::image::Format::Png).unwrap();
            let roundtrip = crate::image::decode(&png).unwrap();
            assert_eq!(roundtrip.to_rgb8(), jpeg.to_rgb8());
            std::fs::write(path, png).unwrap();
        }
        let pillow = crate::image::decode(&std::fs::read(png).unwrap()).unwrap();
        let jpeg_rgb = jpeg.to_rgb8();
        let pillow_rgb = pillow.to_rgb8();
        assert_eq!(jpeg_rgb.len(), pillow_rgb.len());
        let changed_rgb = jpeg_rgb
            .iter()
            .zip(&pillow_rgb)
            .filter(|(a, b)| a != b)
            .count();
        let jpeg_prepared = prepare(&jpeg).unwrap();
        let pillow_prepared = prepare(&pillow).unwrap();
        let changed_prepared = jpeg_prepared
            .chw
            .iter()
            .zip(&pillow_prepared.chw)
            .filter(|(a, b)| a != b)
            .count();
        eprintln!(
            "page27 native JPEG versus Pillow: decoded RGB {changed_rgb}/{}, prepared CHW {changed_prepared}/{}",
            jpeg_rgb.len(),
            jpeg_prepared.chw.len()
        );
        assert!(changed_rgb > 0 && changed_prepared > 0);
    }

    #[test]
    #[ignore = "requires Pillow-decoded page 07 PNG and local SAM graph/runtime"]
    fn native_page07_pillow_pixels_match_saved_mask() {
        let runtime = std::env::var("SAM_TS_RUNTIME").expect("SAM_TS_RUNTIME is required");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let source = std::env::var("SAM_TS_DECODED_PNG").expect("SAM_TS_DECODED_PNG is required");
        let page = crate::image::decode(&std::fs::read(source).unwrap()).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l");
        let inferred = infer_cpu(&page, &root.join("artifacts/proof")).unwrap();
        let saved = root.join("../chapter-combo/.cache/live-chapter-109/sam/masks/07.png");
        let reference = crate::image::decode(&std::fs::read(saved).unwrap()).unwrap();
        let changed = inferred
            .mask
            .iter()
            .zip(&reference.data)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            changed, 0,
            "native ONNX CPU versus saved PyTorch/MPS mask differs at {changed} pixels with exact prepared RGB"
        );
    }

    #[test]
    #[ignore = "requires local chapter scans, ONNX graphs, and 45 full SAM inferences"]
    fn native_cpu_chapter_109_mask_regression() {
        let runtime = std::env::var("SAM_TS_RUNTIME").expect("SAM_TS_RUNTIME is required");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let sources = std::env::var("SAM_TS_CHAPTER_DIR").expect("SAM_TS_CHAPTER_DIR is required");
        let decoded = std::env::var("SAM_TS_DECODED_DIR").ok();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l");
        let proof = root.join("artifacts/proof");
        let saved = root.join("../chapter-combo/.cache/live-chapter-109/sam/masks");
        let mut differences = Vec::new();
        for page_number in 1..=45 {
            let stem = format!("{page_number:02}");
            let page = if let Some(dir) = &decoded {
                crate::image::decode(
                    &std::fs::read(Path::new(dir).join(format!("{stem}.png"))).unwrap(),
                )
                .unwrap()
            } else {
                let source =
                    std::fs::read(Path::new(&sources).join(format!("{stem}.jpg"))).unwrap();
                crate::image::foreign::decode(&source).unwrap()
            };
            let inferred = infer_cpu(&page, &proof).unwrap();
            let reference =
                crate::image::decode(&std::fs::read(saved.join(format!("{stem}.png"))).unwrap())
                    .unwrap();
            let changed = inferred
                .mask
                .iter()
                .zip(reference.data.iter())
                .filter(|(a, b)| a != b)
                .count();
            eprintln!(
                "page {stem}: {changed} differing pixels, load {:?}, encoder {:?}, head {:?}",
                inferred.load, inferred.encoder, inferred.head
            );
            if changed != 0 {
                differences.push((stem, changed));
            }
        }
        assert!(
            differences.is_empty(),
            "native CPU versus saved MPS masks: {differences:?}"
        );
    }

    #[test]
    #[ignore = "requires page 27, local graphs/runtime, and Python ORT stage tensors"]
    fn diagnose_page27_cpu_stages() {
        let runtime = std::env::var("SAM_TS_RUNTIME").expect("SAM_TS_RUNTIME is required");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/sam-ts-l");
        let stage = PathBuf::from(std::env::var("SAM_TS_STAGE_DIR").unwrap());
        let png = std::env::var("SAM_TS_DECODED_PNG").unwrap();
        let page = crate::image::decode(&std::fs::read(png).unwrap()).unwrap();
        let prepared = prepare(&page).unwrap();
        let expected = npy_payload(&std::fs::read(stage.join("prepared.npy")).unwrap()).to_vec();
        assert_eq!(expected.len(), prepared.chw.len() * 4);
        let changed = prepared
            .chw
            .iter()
            .zip(expected.chunks_exact(4))
            .filter(|(a, b)| a.to_le_bytes() != **b)
            .count();
        eprintln!("page27 prepared samples differing from Python: {changed}");
        assert_eq!(changed, 0);

        fn compare(name: &str, actual: &[f32], stage: &Path) -> usize {
            let bytes = std::fs::read(stage.join(format!("{name}.npy"))).unwrap();
            let expected = npy_payload(&bytes);
            assert_eq!(actual.len() * 4, expected.len());
            let mut changed = 0;
            let mut max = 0f32;
            let mut mean = 0f64;
            let mut changed_sign = 0;
            for (&a, b) in actual.iter().zip(expected.chunks_exact(4)) {
                let reference = f32::from_le_bytes(b.try_into().unwrap());
                let delta = (a - reference).abs();
                changed += usize::from(delta != 0.0);
                max = max.max(delta);
                mean += f64::from(delta);
                changed_sign += usize::from((a > 0.0) != (reference > 0.0));
            }
            eprintln!(
                "page27 {name}: changed {changed}, max {max}, mean {}, sign {changed_sign}",
                mean / actual.len() as f64
            );
            changed_sign
        }

        let graph = root.join("artifacts/proof");
        let mut encoder = ort::session::Session::builder()
            .unwrap()
            .with_intra_threads(4)
            .unwrap()
            .commit_from_file(graph.join(ENCODER))
            .unwrap();
        let input = ort::value::Tensor::from_array(([1usize, 3, SIDE, SIDE], prepared.chw.clone()))
            .unwrap();
        let output = encoder.run(ort::inputs!["prepared_rgb" => input]).unwrap();
        let (_, embedding) = output["embedding"].try_extract_tensor::<f32>().unwrap();
        compare("embedding", embedding, &stage);
        let embedding = embedding.to_vec();
        drop(output);
        drop(encoder);
        let mut head = ort::session::Session::builder()
            .unwrap()
            .with_intra_threads(4)
            .unwrap()
            .commit_from_file(graph.join(HEAD))
            .unwrap();
        let input = ort::value::Tensor::from_array(([1usize, 256, 64, 64], embedding)).unwrap();
        let output = head.run(ort::inputs!["embedding" => input]).unwrap();
        let (_, logits) = output["high_res_logits"]
            .try_extract_tensor::<f32>()
            .unwrap();
        assert_eq!(compare("logits", logits, &stage), 0);
        eprintln!(
            "page27 native logit at (463,162): {}",
            logits[463 * SIDE + 162]
        );
        let mask = restore(logits, &prepared).unwrap();
        let expected_mask = npy_payload(&std::fs::read(stage.join("mask.npy")).unwrap()).to_vec();
        let differing: Vec<_> = mask
            .iter()
            .zip(expected_mask)
            .enumerate()
            .filter_map(|(i, (a, b))| {
                (a != &b).then_some((
                    i % prepared.original_width,
                    i / prepared.original_width,
                    *a,
                    b,
                ))
            })
            .collect();
        eprintln!("page27 Rust versus Python ORT CPU mask: {differing:?}");
        assert!(differing.is_empty());
    }
}
