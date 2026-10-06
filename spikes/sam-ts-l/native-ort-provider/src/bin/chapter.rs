//! One-session, 45-page Rust/ORT WebGPU corpus probe. Input manifest is created
//! from Pillow-prepared JPEGs; verify_chapter.py does mask comparisons.
use anyhow::{bail, Context, Result};
use ort::{environment::Environment, session::Session, value::Tensor};
use serde_json::{json, Value};
use std::{fs, path::{Path, PathBuf}, sync::{Arc,atomic::{AtomicBool,Ordering}}, time::{Duration,Instant}};

fn npy(path: &Path) -> Result<Vec<f32>> {
    let bytes=fs::read(path)?;
    if !bytes.starts_with(b"\x93NUMPY") { bail!("not NPY: {}",path.display()); }
    let (offset,len)=match bytes[6] {1=>(10,u16::from_le_bytes([bytes[8],bytes[9]]) as usize),2|3=>(12,u32::from_le_bytes(bytes[8..12].try_into()?) as usize),_=>bail!("NPY version")};
    let header=std::str::from_utf8(&bytes[offset..offset+len])?;
    if !header.contains("'<f4'") || header.contains("'fortran_order': True") {bail!("unsupported NPY header");}
    let data=&bytes[offset+len..]; if data.len()!=3*1024*1024*4 {bail!("prepared input size");}
    Ok(data.chunks_exact(4).map(|x|f32::from_le_bytes(x.try_into().unwrap())).collect())
}
#[cfg(target_os="macos")]
fn metal_bytes() -> Option<u64> {
    use std::ffi::{c_char,c_void};
    #[link(name="Metal",kind="framework")]
    unsafe extern "C" {fn MTLCreateSystemDefaultDevice()->*mut c_void;}
    #[link(name="objc")]
    unsafe extern "C" {fn sel_registerName(name:*const c_char)->*mut c_void;fn objc_msgSend(receiver:*mut c_void,selector:*mut c_void)->usize;}
    unsafe {let d=MTLCreateSystemDefaultDevice();if d.is_null(){return None;}let value=objc_msgSend(d,sel_registerName(c"currentAllocatedSize".as_ptr())) as u64;let _=objc_msgSend(d,sel_registerName(c"release".as_ptr()));Some(value)}
}
#[cfg(not(target_os="macos"))]
fn metal_bytes()->Option<u64>{None}
fn rss() -> u64 {
    #[repr(C)]struct Tv{sec:i64,usec:i32}
    #[repr(C)]struct Usage{utime:Tv,stime:Tv,maxrss:i64,rest:[u8;240]}
    unsafe extern "C"{fn getrusage(who:i32,usage:*mut Usage)->i32;}
    let mut u=std::mem::MaybeUninit::<Usage>::zeroed();
    unsafe{if getrusage(0,u.as_mut_ptr())==0{u.assume_init().maxrss as u64}else{0}}
}
fn main()->Result<()> {
    let args:Vec<_>=std::env::args_os().skip(1).collect();
    if args.len()!=6 {bail!("usage: chapter <ORT dylib> <WebGPU plugin> <graph-dir> <prepared-manifest.json> <raw-output-dir> <result.json>");}
    let runtime=PathBuf::from(&args[0]).canonicalize()?;
    let plugin=if args[1]=="-" {None} else {Some(PathBuf::from(&args[1]).canonicalize()?)};
    let graph=PathBuf::from(&args[2]).canonicalize()?;
    let manifest=PathBuf::from(&args[3]).canonicalize()?;
    let raw_dir=PathBuf::from(&args[4]);let result=PathBuf::from(&args[5]);
    let pages:Vec<Value>=serde_json::from_slice(&fs::read(&manifest)?)?;
    if pages.len()!=45 {bail!("expected 45 pages; got {}",pages.len());}
    fs::create_dir_all(&raw_dir)?;
    ort::init_from(&runtime)?.with_name("samts-webgpu-chapter").commit();
    let env=Environment::current()?;
    let handle=if let Some(path)=&plugin {Some(env.register_ep_library("samts_webgpu_chapter",path)?)} else {None};
    let count=env.devices().filter(|d|d.ep().ok()==Some("WebGpuExecutionProvider")).count();
    if count==0 {bail!("no WebGPU EP device");}
    let stop=Arc::new(AtomicBool::new(false));let thread_stop=Arc::clone(&stop);
    let sampler=std::thread::spawn(move||{let mut max=None;while !thread_stop.load(Ordering::Relaxed){if let Some(v)=metal_bytes(){max=Some(max.map_or(v,|x:u64|x.max(v)));}std::thread::sleep(Duration::from_millis(20));}max});
    let load_start=Instant::now();
    let mut encoder=Session::builder()?.with_intra_threads(4).map_err(|e|anyhow::anyhow!("{e}"))?
        .with_disable_cpu_fallback().map_err(|e|anyhow::anyhow!("{e}"))?
        .with_devices(env.devices().filter(|d|d.ep().ok()==Some("WebGpuExecutionProvider")),None).map_err(|e|anyhow::anyhow!("{e}"))?
        .commit_from_file(graph.join("koharu_samts_encoder.onnx"))?;
    let encoder_load=load_start.elapsed().as_secs_f64()*1000.0;
    let load_start=Instant::now();
    let mut head=Session::builder()?.with_intra_threads(4).map_err(|e|anyhow::anyhow!("{e}"))?
        .with_disable_cpu_fallback().map_err(|e|anyhow::anyhow!("{e}"))?
        .with_devices(env.devices().filter(|d|d.ep().ok()==Some("WebGpuExecutionProvider")),None).map_err(|e|anyhow::anyhow!("{e}"))?
        .commit_from_file(graph.join("koharu_samts_text_head.onnx"))?;
    let head_load=load_start.elapsed().as_secs_f64()*1000.0;
    let mut rows=Vec::new();
    for row in &pages {
        let name=row["page"].as_str().context("page")?;
        let input_path=PathBuf::from(row["prepared"].as_str().context("prepared path")?);
        let input=npy(&input_path)?;
        let start=Instant::now();
        let tensor=Tensor::from_array(([1usize,3,1024,1024],input))?;
        let outputs=encoder.run(ort::inputs!["prepared_rgb"=>tensor])?;
        let (_,embedding)=outputs["embedding"].try_extract_tensor::<f32>()?;
        if embedding.len()!=256*64*64 {bail!("embedding size page {name}");}
        let encoder_ms=start.elapsed().as_secs_f64()*1000.0;
        let tensor=Tensor::from_array(([1usize,256,64,64],embedding.to_vec()))?;
        let outputs=head.run(ort::inputs!["embedding"=>tensor])?;
        let (_,logits)=outputs["high_res_logits"].try_extract_tensor::<f32>()?;
        if logits.len()!=1024*1024 || logits.iter().any(|v|!v.is_finite()) {bail!("logit size/value page {name}");}
        let raw:Vec<u8>=logits.iter().flat_map(|v|v.to_le_bytes()).collect();
        let output=raw_dir.join(format!("{name}.f32le"));fs::write(&output,raw)?;
        let total_ms=start.elapsed().as_secs_f64()*1000.0;
        rows.push(json!({"page":name,"prepared":input_path,"raw_logits":output,"encoder_ms":encoder_ms,"head_and_write_ms":total_ms-encoder_ms,"chain_and_write_ms":total_ms,"process_peak_rss_bytes":rss(),"metal_current_allocated_bytes":metal_bytes()}));
        println!("{name}: {total_ms:.0} ms");
    }
    stop.store(true,Ordering::Relaxed);
    let sampled_peak_metal=sampler.join().unwrap_or(None);
    let peak_rss=rss();
    drop(encoder);drop(head);if let Some(handle)=handle {handle.unregister()?;}
    let summary=json!({"description":"One-session native Rust ORT WebGPU 45-page Pillow-prepared SAM-TS-L corpus probe","runtime":runtime,"plugin":plugin,"graph_dir":graph,"prepared_manifest":manifest,"device_count":count,"provider_library_unregistered_before_exit":plugin.is_some(),"session_load_ms":{"encoder":encoder_load,"text_head":head_load},"memory":{"sampled_peak_metal_current_allocated_bytes":sampled_peak_metal,"sample_interval_ms":20,"peak_process_rss_bytes":peak_rss,"note":"Metal sampled high water can miss transients; unified memory overlaps RSS"},"pages":rows});
    fs::write(&result,serde_json::to_vec_pretty(&summary)?)?;
    println!("{}",result.display());Ok(())
}
