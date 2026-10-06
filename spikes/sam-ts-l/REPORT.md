# M2 SAM-TS-L mask-only ONNX proof

**Status (2026-09-25):** The fixed synthetic page and three local comic pages have FP32 PyTorch-to-ONNX Runtime CPU and Apple WebGPU mask parity: **0 changed restored pixels** on each. Apple MPS also matched all three real-page masks. This is an export/provider proof for these inputs, not a release or quality qualification. This M2 spike changed no app source, UI, catalogue, runtime routing, M0–M1 work, cloud service, manga-ocr, or COO MTSv3; other app edits in the shared worktree predate or are independent of this spike.

## Pinned inputs and forward path

| Item | Identity |
| --- | --- |
| Koharu full checkpoint | `mayocream/koharu-text-sam-ts-l`, revision `5dd97423e0fbf2404264979136d47e8101144046`, `model.safetensors`, 1,355,824,988 bytes, local SHA-256 `bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45` |
| Architecture | Hi-SAM Git commit `69009434d4dba5541f228d8f5acb0754c333d417`; the export script checks HEAD and a clean tracked tree before import |
| Configuration | `vit_l`, one attention-alignment layer, prompt length 12, hierarchical detection disabled; 728/728 state keys strictly loaded, 338,934,897 checkpoint elements |
| Conversion environment | macOS 27.0 arm64, 32 GiB unified memory, Python 3.12.13, PyTorch 2.13.0, ONNX 1.19.0, ONNX Runtime 1.23.2, uv 0.12.10; all Python package versions in `environment.txt` |
| Fixture | Self-generated RGB 701×957 PNG; SHA-256 `55d90fd5daf532b26dbd347ed5f0de82cf1461e1ed1c10bbecf04aad457d9a11` |

The pinned Koharu `inference.py` and our preparation produced identical RGB float32 tensors, with **0 changed values**. The saved normalized tensor also exactly matches `HiSam.preprocess()` on that pinned prepared tensor; see `results/preprocessing.json`. The path is PIL bilinear longest-side resize to 750×1024, RGB-128 top-left canvas to 1024², normalization once in the encoder, threshold at logit > 0, crop to 750×1024, then PIL nearest-neighbor restore to 701×957. The source model's full forward, using `original_size=(1024,1024)`, is the reference. Its high-resolution **logits** are tuple index 3; index 4 is the binary mask.

The downloaded pinned `config.json` SHA-256 is `fcc8213df069f728e0f688c9e26d63330afbeeecd7d5dccca129bce09df68f52`; `inference.py` SHA-256 is `66fe733408dc812ec45fa9adf8f5904f831ab6445f7db5f8b06bbcb40eba1565`.

The encoder graph retains the trained ViT-L image encoder and adapters. The head graph retains the modal aligner, positional encoding derived from the strictly loaded prompt encoder, **all** decoder transformer tokens, shared upscaling, and high-resolution hypernetwork. The source mask decoder's low-resolution mask MLPs, IoU heads, interactive prompt components, and disabled hierarchical decoder cannot feed the high-resolution mask. The manifest lists 676 mask-path checkpoint keys and 52 omitted keys. No generic SAM decoder or OCR weights are used.

## Numerical evidence

The direct-evaluation encoder equals the original checkpoint-wrapped encoder exactly on the fixed tensor. The mask-only PyTorch head equals the full forward's 1,048,576 high-resolution logits exactly; its restored mask also matches exactly. Disabling PyTorch's MHA eval fast path for export changes head logits by at most `1.1444092e-5` on the fixed embedding, with no changed restored pixels.

| FP32 comparison | Max absolute | Mean absolute | RMS | Changed restored pixels |
| --- | ---: | ---: | ---: | ---: |
| Prepared input versus pinned reference | 0 | 0 | 0 | n/a |
| PyTorch versus ORT encoder embedding | `3.78117e-5` | `1.04023e-6` | `1.53048e-6` | n/a |
| PyTorch versus ORT head, same PyTorch embedding | `4.48227e-5` | `4.85095e-6` | `6.82529e-6` | n/a |
| Full PyTorch versus chained ORT logits | `4.50134e-4` | `2.21615e-5` | `2.73316e-5` | **0** |

There are 0 sign disagreements over the 1024² logit canvas; both paths mark 27,666 canvas pixels positive and 24,540 restored pixels positive. The smallest absolute reference logit is `1.96402e-4`. The result is recorded without an invented numerical pass threshold. Exact arrays, restored masks, per-stage output files, and difference metrics are in `fixtures/fp32_parity.npz`, `fixtures/*restored-mask.png`, `results/parity.json`, and ignored local `artifacts/proof/`.

## Graphs and local CPU measurement

`onnx.checker.check_model(..., full_check=True)` passed both graphs. Both are ONNX IR 8, opset 17, static batch 1. Graph package identities and operator inventories are in `results/artifact-manifest.json`.

| Graph | Contract | Package bytes |
| --- | --- | ---: |
| Encoder | RGB float32 `[1,3,1024,1024]` → embedding `[1,256,64,64]` | 1,335,305,985 |
| Text head | Embedding `[1,256,64,64]` → logits `[1,1,1024,1024]` | 22,704,641 |

The combined ONNX package is 1,358,010,626 bytes, **2,185,638 bytes larger** than the full SafeTensors file. Pruning the unreachable heads has not reduced on-disk package size here; the encoder dominates it. Neither graph needs an external-weight sidecar at this size, and the script validates relative sidecars if an exporter emits them.

Parsing the pinned SafeTensors header shows that the 676 live mask-path keys contain **1,352,419,968 of 1,355,739,588 tensor-data bytes (99.755%)**; the 52 omitted keys contain only 3,319,620 bytes. The mask still requires the trained ViT-L encoder, so removing unrelated heads barely changes storage. The source encoder has 24 blocks with 16 attention heads and four global-attention blocks over 64×64 image tokens. A single float32 `[16,4096,4096]` attention array would occupy 1 GiB. This describes the computation's scale, not a measured allocation breakdown. The run did not profile ORT's allocator, so the exact contributions of activations, workspaces, weight copies, and caching to peak RSS remain unknown.

On this Mac, ONNX Runtime CPU with 4 intra-op threads, one warmup and two timed runs measured:

| Stage | First session load | Median warm latency | Peak worker RSS |
| --- | ---: | ---: | ---: |
| Encoder | 517 ms | 11.16 s | 10.41 GB |
| Head | 26.8 ms | 227 ms | 0.51 GB |
| Encoder + head | 546 + 23 ms | 13.81 s | 10.85 GB |

All profiled CPU nodes used `CPUExecutionProvider`. “Cold load” here means the first ORT session in a new process; the OS file cache was not cleared. RSS includes Python and ORT, and the warm chain timing is a separate worker from isolated stage timings. See `results/benchmark-cpu.json` for samples and node counts.

### Real comic pages 07–09

The exported graphs also ran on local `/Users/caved/Downloads/apocalypse/109/{07,08,09}.jpg` (each 690×1600). Each page had a separately saved full-checkpoint FP32 PyTorch reference; the timed ONNX process reused one encoder and one head CPU session across all three pages. This was one sequential run with no explicit warmup. Prepared inputs were identical to the saved reference tensors. The highest PyTorch-versus-ONNX logit max-absolute difference was `0.0003347396851` (page 08); every page had **0 changed 1024² canvas threshold signs and 0 changed restored mask pixels**. The three ONNX mask PNGs are byte-identical to their reference PNGs. This establishes parity for these inputs, not mask quality or other pages.

| Page | Preparation | Encoder | Head | Restore | Page total | Cumulative process peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 07 | 12.7 ms | 9.37 s | 204 ms | 0.90 ms | 9.60 s | 6.53 GB |
| 08 | 9.64 ms | 10.04 s | 210 ms | 0.78 ms | 10.27 s | 11.14 GB |
| 09 | 8.25 ms | 10.37 s | 214 ms | 0.84 ms | 10.60 s | 11.14 GB |

Manifest and graph SHA-256 preflight took 665 ms; first encoder/head `InferenceSession` loads took 467/21.6 ms. Measured inside the process, all three pages took 31.65 s including preflight, session load, parity checks, and array/image writes. `/usr/bin/time -l` recorded **32.21 s wall time**, **11,141,447,680 bytes maximum resident set** (11.14 GB decimal, 10.38 GiB), and 11,082,310,704 bytes peak memory footprint for the complete command. This was macOS 27 arm64 on `Mac17,3`, 10 logical CPUs, 32 GiB unified memory, ORT 1.23.2 CPU with 4 intra-op threads. The OS file cache was not cleared. Page RSS values are the process high-water mark so far, not isolated per-page allocations. These numbers include Python and ORT, not total system memory or the prior PyTorch reference processes. The two ONNX files together occupy 1,358,010,626 bytes (1.358 GB decimal) on disk.

A separate `/usr/bin/time -l` rerun of the full PyTorch reference on page 07 took 10.37 s wall time and reached 5,716,623,360 bytes maximum RSS (5.72 GB decimal); it is not included in the ONNX batch time. The raw timer logs and prepared tensors/masks are local in ignored `artifacts/`; `results/real-pages-performance.json` records graph and source hashes, environment, exact per-page timing, parity, process memory, and both external timer readings. Reproduce the ONNX batch with:

```sh
/usr/bin/time -l spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/run_real_pages.py \
  --source-dir '/Users/caved/Downloads/apocalypse/109' --pages 07 08 09
```

The comic-derived images and arrays remain local and are not part of the tracked proof artifacts. This run covers mask prediction and restoration only; it does not include OCR or inpainting.

### Apple GPU and cross-platform provider probes

The CPU export is not a performance win on this Mac. A controlled page-07 comparison used the **same saved prepared tensor**, one warmup and two measured inference passes, four CPU threads where applicable, and separate processes:

| Path | Median warmed inference | Peak process RSS | Additional memory evidence | Changed restored pixels |
| --- | ---: | ---: | --- | ---: |
| Full PyTorch CPU | 4.18 s | 5.68 GB | macOS peak memory footprint 5.62 GB | 0 |
| Full PyTorch MPS | 2.64 s | 2.96 GB | Sampled Metal driver allocation 5.53 GB; macOS peak memory footprint 6.78 GB | 0 |
| Mask-only ONNX Runtime CPU | 11.18 s | 10.87 GB | macOS peak memory footprint 10.88 GB | 0 |
| Mask-only ONNX Runtime WebGPU | 2.84 s | 2.55 GB | macOS peak memory footprint 7.13 GB; GPU allocation not sampled | 0 |

The PyTorch MPS probe disables CPU fallback, synchronizes Metal, and includes the logits transfer back to a host NumPy array inside each inference timer. Its warmup took 2.74 s; the 2.64 s figure is a median of the next two passes. Pages 08 and 09 measured 2.59 and 2.62 s median warmed MPS inference; all three restored masks had zero changed pixels versus the FP32 CPU reference. MPS logits differed by at most `0.0003061295` across these pages. The WebGPU page-07 probe also used one warmup and two timed passes on the same saved input; its encoder/head median times were 2.72/0.128 s, and all profiled optimized nodes ran on WebGPU. On Apple silicon, process RSS and Metal driver allocation overlap in unified memory and must not be summed. The MPS memory sampler polls at 20 ms, so it may miss a transient maximum. Full measurements are in `results/torch-cpu-page07.json`, `results/ort-cpu-page07.json`, `results/ort-webgpu-page07.json`, `results/torch-mps-page{07,08,09}.json`, and `results/controlled-page07-external-timer.json`; the raw `/usr/bin/time -l` logs remain local under ignored `artifacts/`.

An isolated ONNX Runtime 1.28.0 plus native WebGPU plugin 0.4.0 environment successfully executed both graphs on the Apple M5. The synthetic chained graph took **4.19 s median warm**, with zero changed restored mask pixels; separate ORT profile events assigned all 1,818 optimized encoder nodes and all 386 optimized head nodes to `WebGpuExecutionProvider`, with none assigned to CPU. On real pages 07–09, one reused WebGPU session pair measured page totals of **3.09, 2.89, and 3.04 s**. The three-page in-process total was **10.13 s** including graph/hash preflight, session loads, per-page parity checks, and artifact writes. Its separate post-run profiling took 3.79 s, so `/usr/bin/time -l` recorded 14.12 s for the entire proof command. The real-page process reached **2.79 GB peak RSS** and **7.19 GB macOS peak memory footprint**; GPU allocation was not measured independently. All three WebGPU restored masks were byte-identical to the PyTorch reference PNGs, and each had zero canvas threshold sign changes. Exact hashes, timing, parity, and provider event counts are in `results/real-pages-webgpu-performance.json`; package versions are in `results/environment-webgpu.txt`. The conversion/reference environment was not modified.

Two subsequent three-page WebGPU runs on the same inputs and graph hashes took **14.91 s** and **10.28 s** inside the process, with per-page totals respectively **4.49/4.52/4.11 s** and **2.89/3.11/3.20 s**. Both retained zero changed restored pixels and all profiled nodes assigned to WebGPU. The slower run is recorded, not discarded; its cause was not isolated. Their `/usr/bin/time -l` walls including the separate profile pass were 19.44 and 14.38 s. See `results/real-pages-webgpu-repeat2.json` and `results/real-pages-webgpu-repeat3.json`. A user-facing speed budget should account for this run-to-run spread.

CoreML remains unqualified for the encoder. Its default configuration allowed GPU use but timed out after 180 s during encoder setup. An explicit `MLProgram` + `CPUAndGPU` + static-input attempt again timed out after 120 s, before any encoder inference; it reported 65 candidate CoreML partitions supporting 1,630 of 1,818 optimized nodes. See `results/benchmark-coreml.json` and `results/benchmark-coreml-mlprogram-gpu.json`. The earlier CoreML head alone changed four restored pixels. WebGPU is the successful Apple ONNX provider in this local experiment.

The proof harness now recognizes CPU, CoreML (macOS), CUDA (Windows/Linux), DirectML (Windows), WebGPU (macOS/Windows/Linux), and MIGraphX (Linux), and records unavailable providers as skips. Windows and Linux GPU execution, dependencies, memory, and parity remain **untested**; provider selection in the harness is not desktop model integration. The existing Rust application already has separate ONNX Runtime provider download/selection machinery for its current models. This M2 work has not changed its routing or added SAM-TS-L to the app. The official [ONNX Runtime execution-provider guide](https://onnxruntime.ai/docs/execution-providers/) describes provider priority and CPU fallback; provider presence alone is not evidence that this graph used a GPU.

| Target host | Probe route for this graph | Current evidence |
| --- | --- | --- |
| Apple Silicon macOS | WebGPU; CoreML diagnostic | WebGPU encoder/head and three real pages qualified locally; CoreML encoder times out |
| Windows NVIDIA | CUDA, DirectML or WebGPU | Probe code available; no Windows execution yet |
| Windows AMD/Intel | DirectML or WebGPU | Probe code available; no Windows execution yet |
| Linux NVIDIA | CUDA or WebGPU | Probe code available; no Linux execution yet |
| Linux AMD/Intel | MIGraphX where available, or WebGPU | Probe code available; no Linux execution yet |

The standalone `infer_mask.py` selector now exposes explicit `torch-cpu`,
`torch-mps`, `torch-cuda`, `ort-cpu`, `ort-coreml`, `ort-cuda`, `ort-directml`,
`ort-webgpu`, and `ort-migraphx` choices for a fresh image. Its availability
listing retains inapplicable choices with a reason; selection never changes
to another backend silently. `probe_torch.py` verifies the full pinned
checkpoint/source before strict loading and writes a restored PNG plus timing,
memory, and optional parity JSON. `infer_ort.py` verifies both exported graph
packages, records provider assignment in each graph, and writes the same
fresh-image output types. A fresh synthetic PyTorch CPU run matched the saved
reference mask with zero changed pixels, and its optional reference comparison
had zero logit difference. A fresh-page WebGPU ONNX run on page 07 again had
zero changed restored pixels, 1,818 encoder and 386 head WebGPU node events,
and zero CPU fallback nodes. These are local proof tools, not the desktop
application's SAM-TS-L model route. The current application does not yet
offer any of these SAM-specific choices; doing so requires the later mask
contract, model installation and runtime integration. Windows/Linux GPU
choices are executable probes when their providers are installed, but remain
unqualified until run on those machines. The Mac availability snapshot is
`results/backend-options-mac.json`; it distinguishes an installed ONNX EP
from a verified device and from actual graph execution.

The unified selector was exercised again on comic page 07 using
`--backend torch-mps` and `--backend ort-webgpu`, each with the saved page
reference. Both wrote masks with zero changed restored pixels. The selector's
MPS median after one warmup was 2.535 s, with 2.958 GB process peak RSS and
5.534 GB maximum sampled Metal driver allocation. Its WebGPU inference took
2.543 s encoder plus 0.129 s head, with 2.786 GB process peak RSS before
the separate profiling pass; the run took 7.199 s inside the process when
hash verification, session load and profiling are included. Profiling lifted
that command's peak RSS to 3.991 GB. Exact outputs are in
`results/selector-torch-mps-page07.json` and
`results/selector-webgpu-page07.json`; these additional samples do not
replace the controlled comparison table above.

### CoreML provider observation

The same local ONNX Runtime installation exposes `CoreMLExecutionProvider`, but this graph pair is **not CoreML-qualified**. Encoder and chained workers both exceeded the benchmark's 180-second bound. ONNX Runtime logged 165 candidate CoreML partitions, with 1,414 of 1,818 encoder nodes supported; this is a capability report, not successful execution. No trustworthy CoreML encoder or chain latency/peak-memory figure was obtained.

The head alone loaded in 3.45 s and measured 647 ms median warm latency, 3.53 GB peak worker RSS. ORT profile events show 44 CoreML nodes and 52 CPU fallback nodes. Given the identical PyTorch embedding, its logits differed from the PyTorch reference by max `0.0246868`, mean `0.00709360`, RMS `0.00801462`; **4 canvas threshold signs and 4 restored mask pixels changed** (source pixels `(219,191)`, `(315,238)`, `(231,361)`, `(419,379)`). Relative to CPU ORT on that same embedding, CoreML head logits differed by max `0.0246811` and the same 4 signs. Those differences repeated in a second run. See `results/benchmark-coreml.json`, `results/coreml-head-parity.json`, and `fixtures/coreml_head_logits.npz`. This provider's numerical behavior is distinct from the CPU FP32 proof; no CoreML parity claim is made.

## Export issues resolved

1. The first head trace failed because an embedding made under `torch.inference_mode()` was passed to tracing outside it (`Inference tensors cannot be saved for backward`). Cloning the tensor outside inference mode and tracing under `torch.no_grad()` resolved it.
2. The next head trace failed on `aten::_native_multi_head_attention` at opset 17. Disabling PyTorch's eval MHA fast path **only during the decomposed-head comparison and head export** produced ordinary supported ONNX operators. The original fast path is restored afterward; its FP32 output difference is recorded above.

No unresolved operator blocks this fixed-input mask-only CPU export. CoreML encoder initialization/partitioning and head numerical differences are provider-specific blockers. The model's real-page mask quality, Rust preprocessing/restoration parity, broader shape handling, and redistribution rights remain separate gates. `PROVENANCE.md` records the model card's license and training-lineage claims plus unresolved commercial distribution and hosted-use questions; public download and conversion do not settle them.

## Reproduce

From the repository root:

```sh
bash spikes/sam-ts-l/fetch_sources.sh
bash spikes/sam-ts-l/setup_env.sh
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/make_fixture.py
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/export_mask.py --input spikes/sam-ts-l/fixtures/synthetic_page.png --phase reference
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/export_mask.py --input spikes/sam-ts-l/fixtures/synthetic_page.png --phase export
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/export_mask.py --input spikes/sam-ts-l/fixtures/synthetic_page.png --phase parity
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/benchmark.py --parity-confirmed --provider cpu --repeats 2 --threads 4
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/benchmark.py --parity-confirmed --provider coreml --repeats 2 --threads 4 --timeout-seconds 180
```

The exact commands used for this run also include the `--phase export-head` retry after the private MHA operator failure and `--provider coreml` benchmarking. The local graphs, full checkpoint, source clone, and uncompressed fixtures stay in ignored `artifacts/`; the compact manifest and parity evidence are tracked beside this report.

## Files created

- Conversion and measurement: `export_mask.py`, `benchmark.py`, `run_real_pages.py`, `probe_torch_mps.py`, `probe_ort_page.py`, `fetch_sources.sh`, `setup_env.sh`, `make_fixture.py`, `environment.txt`, `README.md`.
- Evidence: `fixtures/synthetic_page.png`, `fixtures/fp32_parity.npz`, `fixtures/coreml_head_logits.npz`, restored mask PNGs, and JSON files in `results/` (`artifact-manifest`, `preprocessing`, `parity`, `checkpoint-direct`, `benchmark-cpu`, `benchmark-coreml`, `benchmark-coreml-mlprogram-gpu`, `benchmark-webgpu-*`, `coreml-head-parity`, `graph-identity`, `real-pages-performance`, `real-pages-webgpu-performance`, page-07 CPU/ORT/WebGPU probes, page-07–09 MPS probes, and `environment-webgpu.txt`).
- Interpretation and rights: this `REPORT.md` and `PROVENANCE.md`. Large weights, graphs, uncompressed arrays, and command logs are in ignored local `artifacts/`.

## Next experiment

Run the same graph-hash-checked provider harness on actual Windows NVIDIA/DirectML and Linux NVIDIA/AMD/Intel hosts, measuring node assignment, logit/mask parity, GPU memory, cold load, and warm latency separately. Keep CPU parity as a fallback reference. Test the Apple WebGPU graph from the intended Rust/ONNX Runtime package with exact Rust preprocessing/restoration; the Python probe establishes graph viability but not desktop integration. Wider rights-cleared page quality and distribution rights remain separate gates. CoreML encoder compilation is an optional isolated diagnosis now that WebGPU runs locally. The subsequent independent COO MTSv3 chapter probe and three-model comparison are in [the combo report](../chapter-combo/REPORT.md).
