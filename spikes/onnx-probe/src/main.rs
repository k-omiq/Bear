//! Phase 0 spikes 2 and 6: what a model's tensor contract is, and what it
//! costs to run.
//!
//! Spike 6 asks for the detector's tensor **names and shapes** to be pinned,
//! because the ONNX export's output order is undocumented and upstream carries a
//! defensive swap. Spike 2 asks for manga-LaMa's **per-crop latency** on the CPU
//! execution provider, because no published figure exists. Both questions are
//! answered by loading a model and looking, so one probe answers both.
//!
//! ```text
//! spike-onnx-probe <model.onnx> [--bench N] [--ep cpu|coreml] [--threads N]
//!                               [--dim name=value]...
//! ```
//!
//! `--dim` fixes a dynamic dimension; without it a dynamic dimension defaults to
//! 1, which is right for a batch axis and wrong for anything else - so the probe
//! prints what it chose.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use ort::value::{TensorElementType, ValueType};

struct Args {
    model: PathBuf,
    bench: usize,
    ep: String,
    threads: Option<usize>,
    dims: BTreeMap<String, i64>,
}

fn parse_args() -> Result<Args> {
    let mut argv = std::env::args().skip(1);
    let mut model = None;
    let mut bench = 0usize;
    let mut ep = "cpu".to_owned();
    let mut threads = None;
    let mut dims = BTreeMap::new();

    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--bench" => bench = argv.next().ok_or_else(|| anyhow!("--bench needs a count"))?.parse()?,
            "--ep" => ep = argv.next().ok_or_else(|| anyhow!("--ep needs a provider"))?,
            "--threads" => threads = Some(argv.next().ok_or_else(|| anyhow!("--threads needs a count"))?.parse()?),
            "--dim" => {
                let spec = argv.next().ok_or_else(|| anyhow!("--dim needs name=value"))?;
                let (name, value) = spec.split_once('=').ok_or_else(|| anyhow!("--dim wants name=value, got {spec}"))?;
                dims.insert(name.to_owned(), value.parse()?);
            }
            other if model.is_none() => model = Some(PathBuf::from(other)),
            other => bail!("unexpected argument {other}"),
        }
    }

    Ok(Args {
        model: model.ok_or_else(|| anyhow!("usage: spike-onnx-probe <model.onnx> [--bench N] [--ep cpu|coreml] [--threads N] [--dim name=value]"))?,
        bench,
        ep,
        threads,
        dims,
    })
}

fn main() -> Result<()> {
    let args = parse_args()?;

    let dylib = cleaner_core::runtime::find(None)
        .context("locating ONNX Runtime; set ORT_DYLIB_PATH or run scripts/fetch-runtime.sh")?;
    cleaner_core::runtime::load(&dylib)?;
    println!("runtime: {}", dylib.display());
    println!("{}", ort::info());

    println!("available: {:?}", cleaner_core::accel::available());
    let wanted = accelerator_named(&args.ep)?;
    let profile = cleaner_core::accel::ModelProfile {
        name: "probe",
        // The probe is not one of the five models, so it borrows the key the
        // interface would name a session under. Nothing renders it here.
        label_key: "models.kind.inpainter",
        // The probe forces whatever it was asked for, so the DFT rule must not
        // silently redirect it - measuring the partitioned case is the point.
        uses_dft: false,
        measured_better: &[],
        unmeasured_candidates: &[],
        // The probe is what *produces* peak figures; it must not be gated by
        // one. An empty row means `choose_within` does not gate at all, which
        // is the only setting under which forcing a provider measures that
        // provider.
        measured_peak_rss: &[],
        partitioned_on: &[],
    };
    let load_started = Instant::now();
    let (mut session, selection) = cleaner_core::accel::open_session(
        &args.model,
        &profile,
        cleaner_core::accel::Preference::Force(wanted),
        args.threads,
    )
    .with_context(|| format!("loading {}", args.model.display()))?;
    if selection.accelerator != wanted {
        println!("!! {wanted:?} was not usable: {:?}", selection.declined);
    }
    println!(
        "session ready in {:.2} s ({:?})",
        load_started.elapsed().as_secs_f64(),
        selection.accelerator
    );

    // ── the tensor contract ────────────────────────────────────────────────
    // Printed as an ordered list, because the order *is* the contract: spike 6
    // exists because the detector's output order is what upstream is unsure of.
    println!("\ninputs:");
    for (i, outlet) in session.inputs().iter().enumerate() {
        println!("  {i}: {} {}", outlet.name(), describe(outlet.dtype()));
    }
    println!("outputs:");
    for (i, outlet) in session.outputs().iter().enumerate() {
        println!("  {i}: {} {}", outlet.name(), describe(outlet.dtype()));
    }

    if args.bench == 0 {
        return Ok(());
    }

    // ── latency ────────────────────────────────────────────────────────────
    let specs: Vec<InputSpec> = session
        .inputs()
        .iter()
        .map(|outlet| InputSpec::resolve(outlet.name(), outlet.dtype(), &args.dims))
        .collect::<Result<_>>()?;
    println!("\nbenching {} run(s) with:", args.bench);
    for spec in &specs {
        println!("  {} {:?}", spec.name, spec.shape);
    }

    // One untimed run first: the first call pays for weight prepacking and, on
    // CoreML, for compiling the model, and reporting that as steady-state
    // latency is how a benchmark becomes a lie.
    let warmup = Instant::now();
    run_once(&mut session, &specs)?;
    println!("first run (excluded): {:.0} ms", warmup.elapsed().as_secs_f64() * 1000.0);

    let mut timings = Vec::with_capacity(args.bench);
    for _ in 0..args.bench {
        let started = Instant::now();
        run_once(&mut session, &specs)?;
        timings.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    timings.sort_by(f64::total_cmp);
    println!(
        "min {:.0} ms | median {:.0} ms | p95 {:.0} ms | max {:.0} ms",
        timings[0],
        timings[timings.len() / 2],
        timings[(timings.len() * 95 / 100).min(timings.len() - 1)],
        timings[timings.len() - 1]
    );
    Ok(())
}

fn accelerator_named(name: &str) -> Result<cleaner_core::accel::Accelerator> {
    use cleaner_core::accel::Accelerator::*;
    Ok(match name {
        "cpu" => Cpu,
        "coreml" => CoreMl,
        "directml" | "dml" => DirectMl,
        "cuda" => Cuda,
        "tensorrt" | "trt" => TensorRt,
        "rocm" => Rocm,
        "openvino" => OpenVino,
        "webgpu" => WebGpu,
        "xnnpack" => Xnnpack,
        other => bail!("unknown execution provider {other}"),
    })
}

fn describe(ty: &ValueType) -> String {
    match ty {
        ValueType::Tensor { ty, shape, dimension_symbols } => {
            let dims: Vec<String> = shape
                .iter()
                .zip(dimension_symbols.iter())
                .map(|(d, sym)| {
                    if *d >= 0 {
                        d.to_string()
                    } else if !sym.is_empty() {
                        sym.to_string()
                    } else {
                        "?".to_owned()
                    }
                })
                .collect();
            format!("{ty:?}[{}]", dims.join(","))
        }
        other => format!("{other:?}"),
    }
}

struct InputSpec {
    name: String,
    shape: Vec<i64>,
    element: TensorElementType,
}

impl InputSpec {
    fn resolve(name: &str, ty: &ValueType, overrides: &BTreeMap<String, i64>) -> Result<Self> {
        let ValueType::Tensor { ty: element, shape, dimension_symbols } = ty else {
            bail!("input {name} is not a tensor");
        };
        let shape = shape
            .iter()
            .zip(dimension_symbols.iter())
            .map(|(d, sym)| {
                if *d >= 0 {
                    *d
                } else {
                    let symbol: &str = sym.as_ref();
                    overrides.get(symbol).or_else(|| overrides.get(name)).copied().unwrap_or(1)
                }
            })
            .collect();
        Ok(Self { name: name.to_owned(), shape, element: *element })
    }

    fn count(&self) -> usize {
        self.shape.iter().product::<i64>() as usize
    }
}

/// Synthetic input, deterministic so two runs of the probe compare. Values are
/// mid-range rather than zero: a tensor of zeros can take a different path
/// through sparsity-aware kernels and would not measure the real cost.
fn run_once(session: &mut ort::session::Session, specs: &[InputSpec]) -> Result<()> {
    let mut values: Vec<(std::borrow::Cow<'_, str>, ort::session::SessionInputValue<'_>)> = Vec::new();
    for spec in specs {
        let shape: Vec<usize> = spec.shape.iter().map(|d| *d as usize).collect();
        let value = match spec.element {
            TensorElementType::Float32 => {
                let data: Vec<f32> = (0..spec.count()).map(|i| ((i % 251) as f32) / 251.0).collect();
                ort::value::Tensor::from_array((shape, data))?.into_dyn()
            }
            TensorElementType::Int64 => {
                // The bubble detector's `orig_target_sizes`. A plausible value
                // rather than a counter: the postprocessor divides by it.
                let data: Vec<i64> = (0..spec.count()).map(|_| 640).collect();
                ort::value::Tensor::from_array((shape, data))?.into_dyn()
            }
            TensorElementType::Uint8 => {
                let data: Vec<u8> = (0..spec.count()).map(|i| (i % 251) as u8).collect();
                ort::value::Tensor::from_array((shape, data))?.into_dyn()
            }
            other => bail!("input {} has element type {other:?}, which the probe does not synthesise", spec.name),
        };
        values.push((spec.name.clone().into(), value.into()));
    }
    let _ = session.run(values)?;
    Ok(())
}
