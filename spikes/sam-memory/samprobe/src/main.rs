// usage: samprobe <dylib> <cpu|webgpu> <encoder> <head> <cache_mode|-> <out_prefix> <in.f32>...
use std::time::Instant;

#[repr(C)]
struct RUsageV4 { uuid: [u8; 16], f: [u64; 36] }
extern "C" { fn proc_pid_rusage(pid: i32, flavor: i32, buf: *mut RUsageV4) -> i32; }
fn footprint() -> (f64, f64) {
    let mut r = RUsageV4 { uuid: [0; 16], f: [0; 36] };
    unsafe { proc_pid_rusage(std::process::id() as i32, 4, &mut r) };
    // phys_footprint index 7, lifetime_max_phys_footprint index 28 (after uuid)
    (r.f[7] as f64 / (1u64 << 30) as f64, r.f[28] as f64 / (1u64 << 30) as f64)
}

fn session(backend: &str, path: &str, cache: &str) -> ort::session::Session {
    let mut b = ort::session::Session::builder().unwrap().with_intra_threads(4).unwrap();
    if backend == "webgpu" {
        let env = ort::environment::Environment::current().unwrap();
        let devices: Vec<_> = env.devices().filter(|d| d.ep().ok() == Some("WebGpuExecutionProvider")).collect();
        assert!(!devices.is_empty(), "no webgpu device");
        b = b.with_disable_cpu_fallback().unwrap();
        let mut opts = Vec::new();
        if cache != "-" { opts.push(("storageBufferCacheMode".to_string(), cache.to_string())); }
        b = b.with_devices(devices, Some(&opts)).unwrap();
    } else {
        b = b.with_memory_pattern(cache == "-").unwrap()
            .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(cache == "-").build()]).unwrap();
    }
    b.commit_from_file(path).unwrap()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    ort::init_from(&a[1]).unwrap().with_name("samprobe").commit();
    let (backend, cache) = (a[2].as_str(), a[5].as_str());
    let t = Instant::now();
    let mut enc = session(backend, &a[3], cache);
    let mut head = session(backend, &a[4], cache);
    println!("load {:.1}s footprint {:.2} peak {:.2}", t.elapsed().as_secs_f64(), footprint().0, footprint().1);
    for (i, input) in a[7..].iter().enumerate() {
        let bytes = std::fs::read(input).unwrap();
        let x: Vec<f32> = bytes.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        let t = Instant::now();
        let tensor = ort::value::Tensor::from_array(([1usize, 3, 1024, 1024], x)).unwrap();
        let out = enc.run(ort::inputs!["prepared_rgb" => tensor]).unwrap();
        let (_, e) = out["embedding"].try_extract_tensor::<f32>().unwrap();
        let e = e.to_vec(); drop(out);
        let tensor = ort::value::Tensor::from_array(([1usize, 256, 64, 64], e)).unwrap();
        let out = head.run(ort::inputs!["embedding" => tensor]).unwrap();
        let (_, l) = out["high_res_logits"].try_extract_tensor::<f32>().unwrap();
        let secs = t.elapsed().as_secs_f64();
        let raw: Vec<u8> = l.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(format!("{}_{i}.f32", a[6]), raw).unwrap();
        let (now, peak) = footprint();
        println!("page {i} {secs:.2}s footprint {now:.2} peak {peak:.2}");
    }
}
