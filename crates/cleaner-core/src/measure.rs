//! Timing one model on one provider, on this machine.
//!
//! Every figure in [`crate::accel`] was taken on one Apple M5, and the module
//! says so to every other machine. This is how a machine takes its own: open
//! the model's graphs on the provider exactly as an automatic run would, feed
//! them synthetic tensors of the shapes they declare, and time both halves of
//! the cost - the session build, which a run pays once, and the inference,
//! which it pays per page or per region.
//!
//! The method is `spike-onnx-probe`'s, moved here so the application and the
//! probe measure the same way:
//!
//! - **The first run is reported and excluded.** It pays for weight
//!   prepacking and, on CoreML and TensorRT, for compiling the graph.
//! - **Inputs are mid-range, not zero.** A tensor of zeros can take a
//!   different path through sparsity-aware kernels.
//! - **Runs continue until there are three and a second and a half of them**,
//!   capped at ten, and the median is the figure. One run of a 3 ms model is
//!   noise; ten runs of a 3 s one is a minute nobody asked for.

use std::path::Path;
use std::time::{Duration, Instant};

use ort::value::{TensorElementType, ValueType};

use crate::accel::{self, Accelerator, ModelProfile};

/// One graph of a model, with the values its dynamic axes take. A model read
/// as two sessions (an encoder and a decoder) is two of these, timed as one.
pub struct Graph<'a> {
    pub path: &'a Path,
    /// `(axis name or input name, value)`. A dynamic axis not named here is 1,
    /// which is right for a batch axis.
    pub dims: &'a [(&'a str, i64)],
}

/// What one model cost on one provider, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    /// Building every session, from file to ready.
    pub load_ms: f64,
    /// The first run of every graph, which [`Timing::median_ms`] excludes.
    pub first_ms: f64,
    pub median_ms: f64,
    pub runs: usize,
}

const MIN_RUNS: usize = 3;
const MAX_RUNS: usize = 10;
const MIN_TIMED: Duration = Duration::from_millis(1500);

/// Time a model's graphs on one provider. `cancelled` is asked between runs.
///
/// The sessions are opened as [`accel::open_session`] opens them under
/// `Automatic` - the CPU fallback kept for what the provider cannot hold - and
/// dropped before this returns, so the next provider starts from the same
/// memory.
pub fn time_graphs(
    graphs: &[Graph<'_>],
    profile: &ModelProfile,
    accelerator: Accelerator,
    intra_threads: Option<usize>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Timing, String> {
    fits(profile, accelerator)?;
    let load = || {
        graphs
            .iter()
            .map(|graph| {
                let model = accel::open_on(graph.path, accelerator, intra_threads)?;
                let specs = model
                    .inputs()
                    .iter()
                    .map(|outlet| InputSpec::resolve(outlet.name(), outlet.dtype(), graph.dims))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((model, specs))
            })
            .collect::<Result<Vec<_>, String>>()
    };
    time_runs(
        load,
        |sessions| {
            for (model, specs) in sessions.iter_mut() {
                let _turn = model.turn();
                run_once(model, specs)?;
            }
            Ok(())
        },
        cancelled,
    )
}

/// Time the Hayai reader on one provider, through its own read: its
/// decoder's inputs are tied to one another (the cache length, the vision
/// length, the rotary tables), which no synthetic tensor reproduces.
///
/// The reader quietly reopens on the CPU when a provider fails at run time
/// ([`crate::gate::hayai::Hayai::read`]); that is an answer here, not a
/// timing, so it is reported as the failure it is.
pub fn time_hayai(
    vision: &Path,
    decoder: &Path,
    tokenizer: &Path,
    accelerator: Accelerator,
    cancelled: &dyn Fn() -> bool,
) -> Result<Timing, String> {
    use crate::gate::hayai::Hayai;
    use crate::mask::Rect;

    fits(&accel::HAYAI, accelerator)?;
    let page = sample_page(640, 160);
    let rect = Rect::new(16, 16, 608, 128);
    time_runs(
        || Hayai::open(vision, decoder, tokenizer, accel::Preference::Force(accelerator)).map_err(|e| e.to_string()),
        |reader| {
            reader.read(&page, rect).map_err(|e| e.to_string())?;
            if reader.selection().accelerator != accelerator {
                return Err("accel.declined.failed".to_owned());
            }
            Ok(())
        },
        cancelled,
    )
}

/// Time the SAM-TS-L lettering mask on one provider, through its own session:
/// it builds its graphs with options of its own and holds a process-wide
/// permit ([`crate::sam_ts::SamTsSession::open_on`]).
///
/// `cancel` is the caller's, so a Stop reaches the wait for SAM's
/// process-wide permit and a run in flight, not only the gaps between runs.
pub fn time_sam(
    graph_dir: &Path,
    accelerator: Accelerator,
    cancel: &crate::sam_ts::Cancellation,
    cancelled: &dyn Fn() -> bool,
) -> Result<Timing, String> {
    use crate::sam_ts::SamTsSession;

    let page = sample_page(1024, 1024);
    time_runs(
        || SamTsSession::open_on(graph_dir, accelerator, cancel),
        |session| session.infer(&page, cancel).map(drop),
        cancelled,
    )
}

/// The memory gate [`accel::choose_within`] applies to a forced provider:
/// one measured to need more than this machine will grant is not opened.
fn fits(profile: &ModelProfile, accelerator: Accelerator) -> Result<(), String> {
    match (profile.peak_on(accelerator), crate::memory::room()) {
        (Some(needed), Some(room)) if needed > room => Err("accel.declined.memory".to_owned()),
        _ => Ok(()),
    }
}

/// Load once, then run until the timing is worth reporting.
pub fn time_runs<R>(
    load: impl FnOnce() -> Result<R, String>,
    mut run: impl FnMut(&mut R) -> Result<(), String>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Timing, String> {
    let started = Instant::now();
    let mut subject = load()?;
    let load_ms = ms(started.elapsed());

    let mut pass = || -> Result<f64, String> {
        let started = Instant::now();
        run(&mut subject)?;
        Ok(ms(started.elapsed()))
    };
    let first_ms = pass()?;
    let mut timings = Vec::with_capacity(MAX_RUNS);
    let timed = Instant::now();
    while timings.len() < MAX_RUNS && (timings.len() < MIN_RUNS || timed.elapsed() < MIN_TIMED) {
        if cancelled() {
            return Err("cancelled".to_owned());
        }
        timings.push(pass()?);
    }
    timings.sort_by(f64::total_cmp);
    Ok(Timing { load_ms, first_ms, median_ms: timings[timings.len() / 2], runs: timings.len() })
}

/// A grey page with dark strokes on it, so a reader has edges to look at.
fn sample_page(width: u32, height: u32) -> crate::image::Raster {
    use crate::image::{BitDepth, ColorMode, Raster};
    let data = (0..height)
        .flat_map(|y| (0..width).map(move |x| if (x / 12 + y / 20) % 3 == 0 && y % 20 < 14 { 24 } else { 236 }))
        .collect();
    Raster {
        width,
        height,
        mode: ColorMode::Gray,
        depth: BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data,
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The best of a model's timings: the lowest median, and among timings within
/// 5% of it - inside run-to-run spread - the cheapest to build. Fixed-order
/// input, so a tie lands the same way every time.
pub fn fastest(timings: &[(Accelerator, Timing)]) -> Option<Accelerator> {
    let best = timings.iter().map(|(_, timing)| timing.median_ms).min_by(f64::total_cmp)?;
    timings
        .iter()
        .filter(|(_, timing)| timing.median_ms <= best * 1.05)
        .min_by(|a, b| a.1.load_ms.total_cmp(&b.1.load_ms))
        .map(|(accelerator, _)| *accelerator)
}

/// What a measurement is a measurement *of*: the loaded runtime library, the
/// devices it found, the NVIDIA driver, and for a CUDA build the toolkit and
/// cuDNN it would use. A timing taken under one of these says nothing about
/// another - a new driver, a second GPU, a CUDA build in place of the DirectML
/// one, a cuDNN that now has the part it lacked - so a saved measurement counts
/// only while this is unchanged. `None` before a runtime is loaded.
pub fn setup() -> Option<String> {
    let runtime = crate::runtime::loaded()?;
    // The path alone is the same for every build the app installs.
    let mut parts = vec![format!("runtime {}", runtime.display())];
    parts.extend(crate::runtime::loaded_identity());
    if let Ok(env) = ort::environment::Environment::current() {
        let mut devices: Vec<String> = env
            .devices()
            .filter_map(|device| {
                let hardware = device.hardware_device();
                Some(format!("{} {:?} {} {}", device.ep().ok()?, hardware.ty(), hardware.vendor().unwrap_or("?"), hardware.id()))
            })
            .collect();
        devices.sort();
        parts.extend(devices);
    }
    let report = crate::cuda::report();
    parts.extend(report.gpus.iter().map(|gpu| format!("nvidia {} {}", gpu.name, gpu.driver)));
    parts.extend(report.install_lines());
    Some(parts.join("\n"))
}

/// One input as the probe feeds it: its declared shape with the dynamic axes
/// filled in.
pub struct InputSpec {
    pub name: String,
    pub shape: Vec<i64>,
    pub element: TensorElementType,
}

impl InputSpec {
    pub fn resolve(name: &str, ty: &ValueType, dims: &[(&str, i64)]) -> Result<Self, String> {
        let ValueType::Tensor { ty: element, shape, dimension_symbols } = ty else {
            return Err(format!("input {name} is not a tensor"));
        };
        let named = |key: &str| dims.iter().find(|(axis, _)| *axis == key).map(|(_, value)| *value);
        let shape = shape
            .iter()
            .zip(dimension_symbols.iter())
            .map(|(d, sym)| if *d >= 0 { *d } else { named(sym.as_ref()).or_else(|| named(name)).unwrap_or(1) })
            .collect();
        Ok(Self { name: name.to_owned(), shape, element: *element })
    }

    fn count(&self) -> usize {
        self.shape.iter().product::<i64>().max(0) as usize
    }
}

/// One run on synthetic, deterministic input, so two measurements compare.
pub fn run_once(session: &mut ort::session::Session, specs: &[InputSpec]) -> Result<(), String> {
    let mut values: Vec<(std::borrow::Cow<'_, str>, ort::session::SessionInputValue<'_>)> = Vec::new();
    for spec in specs {
        let shape: Vec<usize> = spec.shape.iter().map(|d| *d as usize).collect();
        let n = spec.count();
        let value = match spec.element {
            TensorElementType::Float32 => {
                let data: Vec<f32> = (0..n).map(|i| ((i % 251) as f32) / 251.0).collect();
                ort::value::Tensor::from_array((shape, data)).map(|t| t.into_dyn())
            }
            // The bubble detector's `orig_target_sizes`: a plausible value
            // rather than a counter, because the postprocessor divides by it.
            // Token ids take the same value, inside every vocabulary here.
            TensorElementType::Int64 => {
                ort::value::Tensor::from_array((shape, vec![640i64; n])).map(|t| t.into_dyn())
            }
            TensorElementType::Int32 => {
                ort::value::Tensor::from_array((shape, vec![640i32; n])).map(|t| t.into_dyn())
            }
            TensorElementType::Uint8 => {
                let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
                ort::value::Tensor::from_array((shape, data)).map(|t| t.into_dyn())
            }
            TensorElementType::Bool => {
                ort::value::Tensor::from_array((shape, vec![true; n])).map(|t| t.into_dyn())
            }
            other => return Err(format!("input {} has element type {other:?}, which is not synthesised", spec.name)),
        }
        .map_err(|e| e.to_string())?;
        values.push((spec.name.clone().into(), value.into()));
    }
    session.run(values).map(drop).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Times every model on the providers given, against the real files:
    /// the measurement the Runtime screen runs, without the application.
    ///
    /// `MEASURE_RUNTIME` the runtime library, `MEASURE_MODELS` the model
    /// folder, `MEASURE_ON` a comma list of `cpu`, `webgpu`, `coreml`.
    #[test]
    #[ignore = "manual: needs the runtime and the model files"]
    fn measure_every_model_here() {
        let runtime = std::env::var("MEASURE_RUNTIME").expect("MEASURE_RUNTIME");
        let dir = std::path::PathBuf::from(std::env::var("MEASURE_MODELS").expect("MEASURE_MODELS"));
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let on: Vec<Accelerator> = std::env::var("MEASURE_ON").unwrap_or_else(|_| "cpu".into()).split(',')
            .map(|name| match name {
                "webgpu" => Accelerator::WebGpu,
                "coreml" => Accelerator::CoreMl,
                _ => Accelerator::Cpu,
            })
            .collect();
        let never = || false;
        let graph = |name: &'static str, dims: &'static [(&'static str, i64)]| (dir.join(name), dims);
        type Single<'a> = (&'a str, &'a ModelProfile, (std::path::PathBuf, &'a [(&'a str, i64)]));
        let single: [Single<'_>; 4] = [
            ("ctd", &accel::DETECTOR, graph("comictextdetector.onnx", &[])),
            ("rtSmall", &accel::BALLOON, graph("comic-text-and-bubble-detector-detector-v4-s_int8.onnx", &[("N", 1)])),
            ("rtFull", &accel::FULL_RT, graph("detector.onnx", &[("N", 1), ("H", 640), ("W", 640)])),
            ("inpainter", &accel::LAMA, graph("lama-manga.onnx", &[("batch", 1)])),
        ];
        for accelerator in on {
            for (id, profile, (path, dims)) in &single {
                let result = time_graphs(&[Graph { path, dims }], profile, accelerator, None, &never);
                eprintln!("{id} {accelerator:?}: {result:?}");
            }
            let hayai = time_hayai(
                &dir.join("hayai-ocr-vision.onnx"),
                &dir.join("hayai-ocr-decoder.onnx"),
                &dir.join("hayai-ocr-tokenizer.json"),
                accelerator,
                &never,
            );
            eprintln!("hayai {accelerator:?}: {hayai:?}");
            if std::env::var_os("MEASURE_SAM").is_some() {
                eprintln!("samTs {accelerator:?}: {:?}", time_sam(&dir.join("sam-ts-l"), accelerator, &crate::sam_ts::Cancellation::default(), &never));
            }
        }
    }

    fn timing(median_ms: f64, load_ms: f64) -> Timing {
        Timing { load_ms, first_ms: median_ms, median_ms, runs: 3 }
    }

    #[test]
    fn the_fastest_run_wins_and_a_near_tie_goes_to_the_cheaper_build() {
        assert_eq!(fastest(&[]), None);
        let clear = [(Accelerator::Cpu, timing(2540.0, 300.0)), (Accelerator::WebGpu, timing(1311.0, 4000.0))];
        assert_eq!(fastest(&clear), Some(Accelerator::WebGpu));
        // 3% apart is run-to-run spread; the 25 s CoreML build is not.
        let close = [(Accelerator::CoreMl, timing(383.0, 25_000.0)), (Accelerator::WebGpu, timing(395.0, 900.0))];
        assert_eq!(fastest(&close), Some(Accelerator::WebGpu));
    }
}
