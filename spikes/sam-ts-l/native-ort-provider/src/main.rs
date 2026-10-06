//! Native Rust/ORT WebGPU provider qualification on pinned SAM-TS-L page tensors.
//! Usage: cargo run --release -- <libonnxruntime.dylib> <webgpu-plugin.dylib> <proof-dir> <page-dir> <output.json>
use anyhow::{bail, Context, Result};
use ort::{environment::Environment, session::Session, value::Tensor};
use serde_json::{json, Value};
use std::{fs, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

fn npy_f32(path: &Path, shape: &[usize]) -> Result<Vec<f32>> {
    let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if !data.starts_with(b"\x93NUMPY") { bail!("not NPY: {}", path.display()); }
    let (offset, header_len) = match data[6] { 1 => (10, u16::from_le_bytes([data[8], data[9]]) as usize),
        2 | 3 => (12, u32::from_le_bytes(data[8..12].try_into()?) as usize), _ => bail!("NPY version") };
    let header = std::str::from_utf8(&data[offset..offset+header_len])?;
    if !header.contains("'<f4'") || header.contains("'fortran_order': True") { bail!("unsupported NPY header: {header}"); }
    let count: usize = shape.iter().product();
    let payload = &data[offset+header_len..];
    if payload.len() != count*4 { bail!("NPY payload length for {}: {} vs {}", path.display(), payload.len(), count*4); }
    Ok(payload.chunks_exact(4).map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())).collect())
}
fn npy_u8(path: &Path, count: usize) -> Result<Vec<u8>> {
    let data = fs::read(path)?;
    if !data.starts_with(b"\x93NUMPY") { bail!("not NPY"); }
    let (offset, header_len) = match data[6] { 1 => (10, u16::from_le_bytes([data[8],data[9]]) as usize),
        2 | 3 => (12, u32::from_le_bytes(data[8..12].try_into()?) as usize), _ => bail!("NPY version") };
    let header = std::str::from_utf8(&data[offset..offset+header_len])?;
    if !header.contains("'|u1'") { bail!("unsupported mask NPY header: {header}"); }
    let payload = &data[offset+header_len..];
    if payload.len() != count { bail!("mask length mismatch"); }
    Ok(payload.to_vec())
}
fn peak_rss() -> u64 {
    // macOS ru_maxrss is bytes; this is process RSS, not Metal allocation.
    #[repr(C)] struct TimeVal { sec: i64, usec: i32 }
    #[repr(C)] struct RUsage { utime: TimeVal, stime: TimeVal, maxrss: i64, rest: [u8; 240] }
    unsafe extern "C" { fn getrusage(who: i32, usage: *mut RUsage) -> i32; }
    let mut usage = std::mem::MaybeUninit::<RUsage>::zeroed();
    unsafe { if getrusage(0, usage.as_mut_ptr()) == 0 { usage.assume_init().maxrss as u64 } else { 0 } }
}
#[cfg(target_os = "macos")]
fn metal_current_allocated_bytes() -> Option<u64> {
    use std::ffi::{c_char, c_void};
    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" { fn MTLCreateSystemDefaultDevice() -> *mut c_void; }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend(receiver: *mut c_void, selector: *mut c_void) -> usize;
    }
    unsafe {
        let device = MTLCreateSystemDefaultDevice();
        if device.is_null() { return None; }
        let selector = sel_registerName(c"currentAllocatedSize".as_ptr());
        let allocated = objc_msgSend(device, selector) as u64;
        // MTLCreateSystemDefaultDevice follows Create ownership rules.
        let release = sel_registerName(c"release".as_ptr());
        let _ = objc_msgSend(device, release);
        Some(allocated)
    }
}
#[cfg(not(target_os = "macos"))]
fn metal_current_allocated_bytes() -> Option<u64> { None }

fn profile_counts(path: &Path) -> Result<Value> {
    let events: Value = serde_json::from_slice(&fs::read(path)?)?;
    let mut counts = serde_json::Map::new();
    let mut missing = 0;
    for event in events.as_array().context("profile root not array")? {
        if event.get("cat").and_then(Value::as_str) != Some("Node") { continue; }
        if let Some(provider) = event.pointer("/args/provider").and_then(Value::as_str) {
            let entry = counts.entry(provider.to_owned()).or_insert(json!(0));
            *entry = json!(entry.as_u64().unwrap_or(0) + 1);
        } else { missing += 1; }
    }
    Ok(json!({"node_events_by_provider": counts, "node_events_without_provider": missing}))
}
fn run() -> Result<Value> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 5 { bail!("usage: native-ort-provider <ORT dylib> <WebGPU plugin dylib> <proof-dir> <page-dir> <output.json>"); }
    let runtime = PathBuf::from(&args[0]).canonicalize()?;
    let plugin = if args[1] == "-" { None } else { Some(PathBuf::from(&args[1]).canonicalize()?) };
    let proof = PathBuf::from(&args[2]).canonicalize()?; let page = PathBuf::from(&args[3]).canonicalize()?;
    let output_arg = PathBuf::from(&args[4]);
    let output = if output_arg.is_absolute() { output_arg } else { std::env::current_dir()?.join(output_arg) };
    let page_manifest: Value = serde_json::from_slice(&fs::read(page.join("manifest.json"))?)?;
    let graph_manifest: Value = serde_json::from_slice(&fs::read(proof.join("manifest.json"))?)?;
    if page_manifest["revision"] != graph_manifest["revision"] || page_manifest["checkpoint"]["sha256"] != graph_manifest["checkpoint"]["sha256"] { bail!("page and graph provenance differ"); }
    let dims = &page_manifest["input"];
    let (ow, oh) = (dims["original_wh"][0].as_u64().context("width")? as usize, dims["original_wh"][1].as_u64().context("height")? as usize);
    let prepared = npy_f32(&page.join("prepared_rgb_f32.npy"), &[1,3,1024,1024])?;
    let reference = npy_f32(&page.join("reference_logits_f32.npy"), &[1,1,1024,1024])?;
    let reference_mask = npy_u8(&page.join("reference_restored_mask_u8.npy"), ow*oh)?;
    let preflight_rss = peak_rss();
    let preflight_metal = metal_current_allocated_bytes();
    let stop_sampler = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop_sampler);
    let metal_sampler = std::thread::spawn(move || {
        let mut maximum: Option<u64> = None;
        while !stop_for_thread.load(Ordering::Relaxed) {
            if let Some(value) = metal_current_allocated_bytes() {
                maximum = Some(maximum.map_or(value, |old| old.max(value)));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        maximum
    });
    ort::init_from(&runtime)?.with_name("sam-ts-native-provider-probe").commit();
    let env = Environment::current()?;
    let plugin_handle = if let Some(path) = &plugin { Some(env.register_ep_library("samts_webgpu", path)?) } else { None };
    let devices: Vec<_> = env.devices().filter(|device| device.ep().ok() == Some("WebGpuExecutionProvider")).collect();
    if devices.is_empty() { bail!("WebGPU plugin registered but no WebGpuExecutionProvider device discovered"); }
    let device_names: Vec<_> = devices.iter().map(|device| json!({"ep": device.ep().unwrap_or("?"), "vendor": device.ep_vendor().unwrap_or("?"), "hardware_id": device.hardware_device().id()})).collect();
    let profiling = std::env::var_os("SAMTS_NO_PROFILE").is_none();
    let setup = |model: &Path, profile: &Path| -> Result<(Session, f64)> {
        let start = Instant::now();
        let mut builder = Session::builder()?.with_intra_threads(4).map_err(|e| anyhow::anyhow!("{e}"))?
            .with_inter_threads(1).map_err(|e| anyhow::anyhow!("{e}"))?
            .with_disable_cpu_fallback().map_err(|e| anyhow::anyhow!("{e}"))?;
        if profiling { builder = builder.with_profiling(profile).map_err(|e| anyhow::anyhow!("{e}"))?; }
        builder = builder.with_devices(env.devices().filter(|d| d.ep().ok() == Some("WebGpuExecutionProvider")), None).map_err(|e| anyhow::anyhow!("{e}"))?;
        let session = builder.commit_from_file(model)?;
        Ok((session, start.elapsed().as_secs_f64()*1000.0))
    };
    let prefix = output.with_extension("");
    let (mut encoder, encoder_load_ms) = setup(&proof.join("koharu_samts_encoder.onnx"), &prefix.with_extension("encoder-profile"))?;
    let (mut head, head_load_ms) = setup(&proof.join("koharu_samts_text_head.onnx"), &prefix.with_extension("head-profile"))?;
    let session_rss = peak_rss();
    let session_metal = metal_current_allocated_bytes();
    let encoder_input = encoder.inputs()[0].name().to_owned();
    let head_input = head.inputs()[0].name().to_owned();
    let mut timings = Vec::new();
    let mut final_logits = Vec::new();
    for repeat in 0..3 {
        let start = Instant::now();
        let tensor = Tensor::from_array(([1usize,3,1024,1024], prepared.clone()))?;
        let input_tensor_ms = start.elapsed().as_secs_f64()*1000.0;
        let outputs = encoder.run(ort::inputs![encoder_input.as_str() => tensor])?;
        let encoder_run_ms = start.elapsed().as_secs_f64()*1000.0 - input_tensor_ms;
        let (shape, embedding) = outputs[0].try_extract_tensor::<f32>()?;
        if &**shape != &[1,256,64,64] { bail!("embedding shape {shape:?}"); }
        let tensor = Tensor::from_array(([1usize,256,64,64], embedding.to_vec()))?;
        let head_input_ms = start.elapsed().as_secs_f64()*1000.0 - input_tensor_ms - encoder_run_ms;
        let outputs = head.run(ort::inputs![head_input.as_str() => tensor])?;
        let head_run_ms = start.elapsed().as_secs_f64()*1000.0 - input_tensor_ms - encoder_run_ms - head_input_ms;
        let (shape, logits) = outputs[0].try_extract_tensor::<f32>()?;
        if &**shape != &[1,1,1024,1024] { bail!("logits shape {shape:?}"); }
        final_logits = logits.to_vec();
        let chain_ms = start.elapsed().as_secs_f64()*1000.0;
        timings.push(json!({"run":repeat, "input_tensor_ms":input_tensor_ms, "encoder_ort_run_ms":encoder_run_ms, "head_input_tensor_ms":head_input_ms, "head_ort_run_ms":head_run_ms, "output_extract_ms":chain_ms-input_tensor_ms-encoder_run_ms-head_input_ms-head_run_ms, "chain_ms":chain_ms}));
    }
    let inference_rss = peak_rss();
    let inference_metal = metal_current_allocated_bytes();
    stop_sampler.store(true, Ordering::Relaxed);
    let sampled_peak_metal = metal_sampler.join().unwrap_or(None);
    let assignments = if profiling {
        let encoder_profile = encoder.end_profiling()?;
        let head_profile = head.end_profiling()?;
        let counts = json!({"encoder": profile_counts(Path::new(&encoder_profile))?, "text_head": profile_counts(Path::new(&head_profile))?});
        fs::remove_file(&encoder_profile)?; fs::remove_file(&head_profile)?;
        Some(counts)
    } else { None };
    drop(encoder); drop(head);
    if let Some(handle) = plugin_handle { handle.unregister()?; }
    let (mut max_abs, mut sum_abs, mut sum_sq, mut changed, mut sign_changed) = (0f64,0f64,0f64,0u64,0u64);
    for (a,b) in reference.iter().zip(final_logits.iter()) {
        if !b.is_finite() { bail!("nonfinite output logit"); }
        let d = (*a as f64-*b as f64).abs(); max_abs = max_abs.max(d); sum_abs += d; sum_sq += d*d;
        if d != 0.0 { changed += 1; } if (*a > 0.0) != (*b > 0.0) { sign_changed += 1; }
    }
    let logits_path = output.with_extension("logits.f32le");
    let raw_logits: Vec<u8> = final_logits.iter().flat_map(|value| value.to_le_bytes()).collect();
    fs::write(&logits_path, raw_logits)?;
    let provider_counts = |stage: &str| assignments.as_ref().map(|value| value[stage]["node_events_by_provider"]["WebGpuExecutionProvider"].as_u64().unwrap_or(0));
    Ok(json!({"runtime":runtime,"plugin":plugin,"ort_info":ort::info(),"device_names":device_names,"page":page,"graph_dir":proof,"graph_manifest_revision":graph_manifest["revision"],"input_source":dims,"cold_definition":"fresh process first session, OS file cache uncontrolled","timing_ms":{"cold_encoder_session_load":encoder_load_ms,"cold_head_session_load":head_load_ms,"inference":timings},"memory":{"preflight_peak_process_rss_bytes":preflight_rss,"post_session_peak_process_rss_bytes":session_rss,"post_inference_peak_process_rss_bytes":inference_rss,"metal_current_allocated_bytes_at_preflight":preflight_metal,"metal_current_allocated_bytes_after_session_load":session_metal,"metal_current_allocated_bytes_after_inference":inference_metal,"sampled_peak_metal_current_allocated_bytes":sampled_peak_metal,"metal_sample_interval_ms":20,"note":"ru_maxrss is process high-water. MTLDevice currentAllocatedSize sampled every 20 ms from a separate thread may miss transient peak and may not include ORT plugin resources if a different Metal device instance is used. Unified-memory allocations overlap process RSS."},"provider_library_unregistered_before_exit":plugin.is_some(),"profiling_enabled":profiling,"profile_assignments":assignments,"profile_files_removed_after_counting":profiling,"raw_logits_f32le":logits_path,"requested_provider_has_nodes_in_every_graph":match (provider_counts("encoder"), provider_counts("text_head")) { (Some(a),Some(b)) => Some(a>0 && b>0), _ => None },"parity":{"logits":{"shape":[1,1,1024,1024],"max_abs":max_abs,"mean_abs":sum_abs/reference.len() as f64,"rmse":(sum_sq/reference.len() as f64).sqrt(),"changed_elements":changed},"changed_canvas_signs":sign_changed,"changed_restored_pixels":null,"exact_restored_mask_parity":null,"reference_mask_pixels":reference_mask.iter().filter(|&&p|p != 0).count(),"mask_parity_note":"Run verify_native_mask.py for pinned Pillow nearest resize"}}))
}
fn main() {
    match run() {
        Ok(result) => {
            let output_arg = PathBuf::from(std::env::args_os().nth(5).unwrap());
            let output = if output_arg.is_absolute() { output_arg } else { std::env::current_dir().unwrap().join(output_arg) };
            if let Err(error) = fs::write(&output, serde_json::to_vec_pretty(&result).unwrap()) { eprintln!("write: {error}"); std::process::exit(1); }
            println!("{}", output.display());
        },
        Err(error) => { eprintln!("native provider probe failed: {error:#}"); std::process::exit(1); }
    }
}
