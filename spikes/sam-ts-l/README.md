# SAM-TS-L mask export proof

`export_mask.py` reconstructs the pinned Koharu `vit_l` model, verifies the
full safetensors SHA-256, and strictly loads every checkpoint key. It follows
the pinned `inference.py` image preparation and mask restoration. Its two
ONNX outputs cover only the trained encoder/adapters and the learned modal
aligner plus high-resolution text-mask path. The full source model runs for
the independent PyTorch reference.

From the repository root, run `bash spikes/sam-ts-l/fetch_sources.sh` and
`bash spikes/sam-ts-l/setup_env.sh` to obtain and verify the pinned full
checkpoint/source and install the pinned conversion environment. Then run:

```sh
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/export_mask.py \
  --input spikes/sam-ts-l/fixtures/synthetic_page.png --phase reference
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/export_mask.py \
  --input spikes/sam-ts-l/fixtures/synthetic_page.png --phase export
# During head-export troubleshooting, reuse an already exported encoder:
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/export_mask.py \
  --input spikes/sam-ts-l/fixtures/synthetic_page.png --phase export-head
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/export_mask.py \
  --input spikes/sam-ts-l/fixtures/synthetic_page.png --phase parity
```

The default output directory is `artifacts/proof`, which keeps the large local
graphs, weights, and uncompressed arrays out of version control. Compact
copies of the fixture arrays, manifest, parity metrics, and benchmark results
are in `fixtures/` and `results/`; see `REPORT.md` for measurements and
limitations. `manifest.json` records the
strictly loaded keys, the live mask-path key set, omitted keys, ONNX IR/opset,
operators, initializers, external data files, sizes, and hashes. `parity.json`
records actual maximum/mean/RMS differences and changed restored mask pixels;
it does not impose an arbitrary acceptance threshold. Fixed `.npy` arrays
include prepared RGB, normalized RGB, PyTorch embeddings/logits, ONNX stage
outputs, and restored masks. The PNG masks are convenient visual copies.

The encoder input is `[1,3,1024,1024]` RGB float32 in 0–255 with PIL bilinear
resize and top-left placement on RGB-128 canvas. It normalizes internally.
The text head consumes `[1,256,64,64]` embeddings and emits
`[1,1,1024,1024]` float32 logits. Threshold, crop, and nearest-neighbor
restoration remain outside ONNX. There is no OCR dependency.

After reviewing CPU parity, `benchmark.py --parity-confirmed` measures graph
packages, first session load, warm latency, peak process RSS, profiled node
provider assignments, and differences from the saved PyTorch fixture. It
checks current graph hashes against the manifest. The probe supports `cpu`,
`coreml` (macOS), `cuda` (Windows/Linux), `directml` (Windows), `webgpu`
(macOS/Windows/Linux), and `migraphx` (Linux). Repeat `--provider` to select
probes; by default it records all six, marking
providers unavailable on the current machine as `skipped` with a reason.

```sh
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/benchmark.py \
  --parity-confirmed --provider cpu --provider coreml --repeats 2 \
  --timeout-seconds 180 --output spikes/sam-ts-l/artifacts/proof/providers.json
```

Use the Python executable for the target system when running the same script
on Windows or Linux. Use an appropriate ONNX Runtime build in that
environment: `onnxruntime` for CPU/CoreML, `onnxruntime-gpu` with matching
CUDA/cuDNN libraries for CUDA, or `onnxruntime-directml` for DirectML. Native
WebGPU needs a compatible `onnxruntime-ep-webgpu` plugin alongside
`onnxruntime`, or a build with WebGPU compiled in. The script registers the
native plugin and selects its discovered EP devices when available. This is
separate from the application's WebGPU runtime. MIGraphX needs an AMD ROCm
ONNX Runtime build with that EP and compatible Linux GPU drivers. The pinned
conversion environment installs only `onnxruntime`; these other provider
packages and builds are not qualified by this spike. On one Apple M5 with an
isolated ONNX Runtime 1.28.0 native WebGPU plugin, the synthetic fixture ran
through both graphs with all profiled encoder and head nodes assigned to
`WebGpuExecutionProvider` and zero changed restored mask pixels. Pages 07–09
were also checked on that Mac, as recorded in `REPORT.md`; these results do
not qualify other WebGPU machines. CUDA, DirectML, MIGraphX,
and Windows/Linux WebGPU remain untested on matching target hardware. A
provider being listed by ORT or included in `get_providers()` does not prove
acceleration: read
`requested_provider_has_profiled_nodes_by_graph` and `profile_assignments` in
the output. A true value means at least one profiled node event used that
provider; the exact counts also show CPU fallback. `status: completed` means
the worker finished, while exact restored-mask parity has its own boolean.
`peak_process_rss_bytes` measures host process memory before parity and the
separate profiling pass; it does not measure GPU memory or Apple unified
memory outside the process. PyTorch fixture parity is reported as measured
differences and changed mask pixels without a universal acceptance threshold.
For a CoreML configuration probe, pass for example
`--coreml-model-format MLProgram --coreml-compute-units CPUAndGPU
--coreml-static-shapes 1`. Add `--stage encoder` to limit a costly probe to
one graph; `--stage` can be repeated. The default CoreML options reproduce
ORT's NeuralNetwork, ALL, dynamic-shape settings. `--timeout-seconds` is a
hard subprocess limit for each provider/stage; a timed-out stage is recorded
in the JSON result and the remaining stages continue. Changing CoreML options can change graph
partitioning and numerical output, so inspect the saved parity and profiled
assignments for each configuration.

`run_real_pages.py` accepts the same six `--provider` names to compare local
pages against saved PyTorch references. CPU remains the default with its
existing output path. Every other provider writes to its own
`artifacts/examples/apocalypse-109-<provider>` directory and reads references
from `artifacts/examples/apocalypse-109`; use `--reference-dir` and
`--output-dir` to choose other paths. The runner's PIL/NumPy preparation and
restoration need no PyTorch installation once the reference files exist. It
checks provider availability, applies DirectML's required session options,
and accepts the benchmark's `--coreml-*` settings. Each non-CPU run profiles
one separate pass per graph after timed pages, records node assignments and
exact mask parity, and keeps profiling overhead separate from per-page
latency and RSS. The available provider list alone does not qualify an
untested accelerator or operating system.

```sh
python spikes/sam-ts-l/run_real_pages.py --source-dir /path/to/pages \
  --provider webgpu --pages 07 08 09
```

The IoU token and all mask tokens remain in the transformer because attention
couples them to the high-resolution token. The low-resolution mask MLPs, IoU
prediction heads, point/box/mask prompts, and hierarchical decoder do not
feed the high-resolution mask. The fixed positional encoding is derived from
the strictly loaded Gaussian matrix and folded into the head graph.

## Reproduce the Apple WebGPU probe

After the pinned graphs and saved PyTorch page references exist, make a
separate ONNX-only environment. The exact versions used on the Apple M5 are
also recorded in `results/environment-webgpu.txt`:

```sh
uv venv spikes/sam-ts-l/artifacts/ort-webgpu-venv --python 3.12
uv pip install --python spikes/sam-ts-l/artifacts/ort-webgpu-venv/bin/python \
  'onnxruntime==1.28.0' 'onnxruntime-ep-webgpu==0.4.0' \
  'onnx==1.19.0' 'numpy==2.3.3' 'pillow==11.3.0'
spikes/sam-ts-l/artifacts/ort-webgpu-venv/bin/python spikes/sam-ts-l/benchmark.py \
  --parity-confirmed --provider webgpu --stage chain --warmups 1 --repeats 2 \
  --timeout-seconds 120 --output spikes/sam-ts-l/artifacts/proof/benchmark-webgpu.json
spikes/sam-ts-l/artifacts/ort-webgpu-venv/bin/python spikes/sam-ts-l/run_real_pages.py \
  --source-dir '/Users/caved/Downloads/apocalypse/109' --pages 07 08 09 --provider webgpu
```

`run_real_pages.py` uses PIL/NumPy preparation and needs no PyTorch import.
It checks the prepared tensor against each saved reference before inference,
compares logits and restored masks afterward, and profiles provider node
assignment in a separate pass. Its WebGPU outputs default to a separate
ignored directory so CPU results remain available. The comic images are local
user-provided inputs and are not part of this repository.

## Select a PyTorch device

`probe_torch.py` runs the **full pinned PyTorch checkpoint** on a fresh image
and saves a binary mask PNG and JSON report. It accepts `cpu` on supported
hosts, `mps` when Apple Metal is
available, and `cuda` when the installed PyTorch build and NVIDIA device make
CUDA available. Use `--list-backends` for machine-readable availability before
offering a device choice. Selection is explicit; an unavailable device fails
before model loading. PyTorch MPS CPU fallback is disabled. CUDA and MPS work
are synchronized before latency is recorded. A CUDA run also reports PyTorch
allocator peaks, which are not a measure of all device memory use.

```sh
python spikes/sam-ts-l/probe_torch.py --list-backends
python spikes/sam-ts-l/probe_torch.py --device cpu --input /path/to/page.jpg
python spikes/sam-ts-l/probe_torch.py --device mps --input /path/to/page.jpg --output-mask /path/to/mask.png
python spikes/sam-ts-l/probe_torch.py --device cuda --cuda-index 0 --input /path/to/page.jpg
# Compare with a saved reference when one exists:
python spikes/sam-ts-l/probe_torch.py --device cpu --page-dir /path/to/page-07 --input /path/to/07.jpg
```

Use the Python executable from the pinned environment on the target system,
with a PyTorch wheel that supports its selected accelerator. The runner uses
the reference's PIL resize, RGB-128 padding, and mask restoration. With
`--page-dir`, it also verifies the source image hash, requires exact equality
with the saved prepared input, and compares logits and the restored mask with
the saved CPU reference. The `--input` path may be omitted in this mode if the
manifest's source path still exists. Without `--page-dir`, parity is `null`.
Every run verifies the full checkpoint hash and source revision and strictly
loads all weights into the fixed `vit_l` architecture. Results report
availability, selected device, timings, process RSS, and optional parity.
Run each device in a separate process for meaningful process RSS high-water comparisons.
`probe_torch_mps.py` remains a compatibility entry point with MPS as its
default. CUDA on Windows and Linux has not been measured in this spike; a
reported available device is a choice to test, not a parity or performance
qualification.

## Choose any mask backend for a fresh image

`infer_mask.py` is one entry point for the two inference engines. It lists
PyTorch CPU/MPS/CUDA and ONNX Runtime CPU/CoreML/CUDA/DirectML/WebGPU/MIGraphX,
including options unavailable on the current operating system and the reason.
PyTorch and ONNX Runtime can use separate Python environments; pass their
executables to the selector. The chosen backend must run as requested. The
ONNX path verifies the pinned graph package and records actual encoder and
head node assignments; a non-CPU provider that executes zero nodes in either
graph fails. CPU fallback nodes, when present, are reported in `run.json`.

```sh
python spikes/sam-ts-l/infer_mask.py --list-backends \
  --torch-python spikes/sam-ts-l/artifacts/venv/bin/python \
  --ort-python spikes/sam-ts-l/artifacts/ort-webgpu-venv/bin/python
python spikes/sam-ts-l/infer_mask.py --backend ort-webgpu \
  --input /path/to/page.jpg --output-dir /path/to/output \
  --ort-python spikes/sam-ts-l/artifacts/ort-webgpu-venv/bin/python
python spikes/sam-ts-l/infer_mask.py --backend torch-mps \
  --input /path/to/page.jpg --output-dir /path/to/output \
  --torch-python spikes/sam-ts-l/artifacts/venv/bin/python
```

The same command accepts `torch-cpu`, `torch-cuda`, `ort-cpu`, `ort-coreml`,
`ort-cuda`, `ort-directml`, or `ort-migraphx` when the corresponding system,
runtime build, device and dependencies are available. To compare a particular
image with a saved PyTorch reference, add `--reference-page-dir` pointing to
its `page-NN` directory. The output is `mask.png` and `run.json`. The four
Mac M5 benchmark rows in `REPORT.md` are local measurements; Windows/Linux
backends remain unmeasured. This standalone Python tool is a proof and is not
the desktop app's model router. SAM-TS-L has not yet been integrated into the
app's cleaning workflow.

## Run a whole chapter with one PyTorch model load

`run_chapter_torch.py` scans supported images in a source directory, sorts
numbered pages numerically, and processes them sequentially with one strictly
verified checkpoint load. It writes one binary mask PNG and JSON report per
page, plus `summary.json` with timing and memory measurements. This is a mask
run only: the output does not contain text boxes, OCR, reading order, or
inpainting. The selected accelerator must be available; MPS CPU fallback is
disabled before PyTorch is imported.

```sh
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/run_chapter_torch.py \
  --source-dir /Users/caved/Downloads/apocalypse/109 \
  --output-dir spikes/sam-ts-l/artifacts/examples/chapter-109-torch-mps \
  --device mps
```

For a quick first-page check, add `--limit 1` and choose a different output
directory. Outputs under `artifacts/` are ignored by Git because they contain
local comic-derived images. The script also accepts `--device cpu` or
`--device cuda` when the corresponding PyTorch build and hardware support it.
The completed chapter 109 run has a labeled `contact-sheet.png`, `masks.zip`
containing all 45 PNGs, and `package.json` in its output directory. Compact
measurements and artifact hashes are in `results/chapter-109-torch-mps.json`.
