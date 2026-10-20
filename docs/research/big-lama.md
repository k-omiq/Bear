# Big LaMa for manga text removal: research

Research note, 19 October 2026. No code was changed. Measurements were made on scratch copies
of the model files with ONNX Runtime 1.28.0, the runtime `scripts/fetch-runtime.sh` pins.

## Go or no-go

**Not now.** The real-page test in section 7 changed the first answer. A tone-level correction
on the shipped manga LaMa beat every Big LaMa variant on screentone: it removes the model's
darkening (about 2.5 levels) and cuts the grey-level error by about 20%. One large pass with the
manga weights smeared screentone more than 512 px tiles did. Do the level correction first.
Revisit a dynamic-size LaMa only after it, with a parity-checked export. The original
photo-trained big-lama weights are a no-go as a manga cleaner.

The first draft of this note said "go, narrowly" for a dynamic-size manga LaMa. That was before
section 7.

## 1. What the app already has

The app already ships a Big LaMa. It is the manga finetune, at a fixed 512 px input.

- `models/lama-manga.onnx` comes from `mayocream/lama-manga-onnx` at revision `b55497a`
  (SHA-256 `4512adab...`, 207,482,644 bytes), pinned in `scripts/fetch-models.sh`,
  `src-tauri/src/weights.rs` and `deploy/cloud/common/manifest.py`.
- Its card says it is the ONNX export of
  [dreMaz/AnimeMangaInpainting](https://huggingface.co/dreMaz/AnimeMangaInpainting), built on
  Carve's `FourierUnitJIT`. The dreMaz card says `lama_large_512px.ckpt` is
  [big LaMa](https://github.com/advimman/lama) finetuned on about 300k manga and anime images.
  "Big" names the model size (51M parameters, 18 FFC residual blocks,
  `docs/research/custom-model-training-feasibility.md`), and the app already has that size.
- The graph (read with the `onnx` package): opset 17, PyTorch 2.7.0 exporter, inputs
  `image [batch,3,512,512]` and `mask [batch,1,512,512]`. Only the batch axis is dynamic. The
  output shape metadata is malformed (`[batch, 3, batch, Sigmoidoutput_dim_3]`) but the tensor
  comes back as 1 x 3 x 512 x 512.
- The desktop tiles regions above 512 px into 512 px crops with 128 px overlap and declines
  regions above 2048 px (`crates/cleaner-core/src/engines/model.rs`, `MODEL_INPUT`,
  `TILE_OVERLAP`, `MAX_BOX`).
- The cloud worker runs the same file on CUDA as the Klein v2 pre-fill, also in 512 px tiles
  (`deploy/cloud/common/flux.py`, `LamaFill`).
- The interface already has a disabled row: `src/lib/model/pipelines.js`, id `big-lama`,
  `files: []`, `ready: false`, note "Larger LaMa for wide areas". `docs/findings.md` lists it as
  having no downloader.
- MI-GAN is no longer an engine. A stored `"migan"` is read as LaMa
  (`src-tauri/src/run.rs:241-251`). The file `models/migan_pipeline_v2.onnx` is still on disk.

### A code fact this research contradicts

`lama-manga.onnx` has **no `DFT` node**. Of its 17,472 nodes the Fourier units are `MatMul`
(216), `Einsum` (216), `Cos` and `Sin` (144 each), with the matrices built at run time from
`Range` (180). That is Carve's matrix-product DFT
([ffc.py](https://github.com/Carve-Photos/lama/blob/main/saicinpainting/training/modules/ffc.py)).
So these statements in the code are wrong about the file:

- `crates/cleaner-core/src/accel.rs`, `LAMA`: "The one with `DFT` in it" and `uses_dft: true`,
  which makes `implements_dft` filter CUDA and CoreML out.
- `crates/cleaner-core/src/engines/model.rs:4`: "with a fixed 512² float input and `DFT` in the".

The CoreML slowness is real, but its likely cause is different: ONNX Runtime's CoreML provider
has no builder for `Einsum`, `Range`, `ConstantOfShape` or `Neg`
([op_builder_factory.cc](https://github.com/microsoft/onnxruntime/blob/main/onnxruntime/core/providers/coreml/builders/op_builder_factory.cc)),
so the graph is split between CoreML and the CPU. This is a follow-up; it was not changed here.

### What "adding Big LaMa" would add

1. **A bigger working size.** One model pass over a whole region up to a cap (1536 px is
   realistic, see section 3), instead of 512 px tiles. LaMa's Fourier units see the whole input
   at once, which is the point of the architecture; tiling cuts that view at 512 px. This is
   the real gain, and it matches the existing "Larger LaMa for wide areas" note.
2. **Optionally, the original photo weights** (advimman big-lama). These are different weights,
   trained on Places, not manga. Nothing public shows they beat the manga finetune on manga.

## 2. Candidates

Sizes and hashes come from the Hugging Face API (`?blobs=true`) or HTTP `Content-Length`.

| Candidate | Weights | Licence | File | Input | Shape | Fourier units |
|---|---|---|---|---|---|---|
| [advimman/lama](https://github.com/advimman/lama) big-lama | original (Places) | Apache-2.0 (LICENSE) | `big-lama.zip`, 381,428,720 bytes on the [smartywu/big-lama](https://huggingface.co/smartywu/big-lama) mirror | PyTorch, RGB 0..1 + mask | any size, multiple of 8 | `torch.fft` |
| [IOPaint](https://github.com/Sanster/IOPaint/blob/main/iopaint/model/lama.py) `big-lama.pt` | original | Apache-2.0 | 205,669,692 bytes, MD5 `e3aa4aaa15225a33ec84f9f4bc47e500` ([release](https://github.com/Sanster/models/releases/download/add_big_lama/big-lama.pt)) | TorchScript | any size, `pad_mod = 8` | `torch.fft` |
| IOPaint `anime-manga-big-lama.pt` | dreMaz finetune | MIT (dreMaz) | 205,717,870 bytes, MD5 `29f284f36a0a510bcacf39ecf4c4d54f` | TorchScript | any size, `pad_mod = 8` | `torch.fft` |
| [dreMaz/AnimeMangaInpainting](https://huggingface.co/dreMaz/AnimeMangaInpainting) | manga finetune | MIT | `lama_large_512px.ckpt`, 204,544,673 bytes, SHA-256 `11d30fbb...` | Lightning checkpoint | any size, multiple of 8 | `torch.fft` |
| [Carve/LaMa-ONNX](https://huggingface.co/Carve/LaMa-ONNX) | original | Apache-2.0 | `lama_fp32.onnx` 208,044,816 bytes (recommended), `lama.onnx` 207,479,252 (dynamo, opset 18, "works slowly") | ONNX, opset 17 | **fixed 512** | matrix products |
| [mayocream/lama-manga-onnx](https://huggingface.co/mayocream/lama-manga-onnx) (shipped) | manga finetune | card tag Apache-2.0, source MIT | 207,482,644 bytes | ONNX, opset 17 | **fixed 512** | matrix products (measured) |
| [ogkalu/lama-manga-onnx-dynamic](https://huggingface.co/ogkalu/lama-manga-onnx-dynamic) | manga finetune (per card) | card tag Apache-2.0, source MIT | `lama-manga-dynamic.onnx`, 206,291,843 bytes, SHA-256 `de31ffa5...`, rev `ee4ed4a` | ONNX, opset 18, IR 8 | **dynamic** `[batch,3,h,w]` | matrix products (measured) |
| [smile-forge/big-lama-onnx](https://huggingface.co/smile-forge/big-lama-onnx) | original | Apache-2.0 | `inpainter/inpainter.onnx`, 208,851,664 bytes, SHA-256 `ef85edd8...`, rev `4211d2c` | ONNX, opset 20, IR 10 | **dynamic**, 64 to 4096, multiple of 8 | matrix products (measured) |
| [Liiesl/lama-manga-onnx-quant](https://huggingface.co/Liiesl/lama-manga-onnx-quant) | manga finetune | Apache-2.0 tag | fp16 weights 108,818,588 bytes, int8 weights 59,809,275 | ONNX, one 4-channel input | fixed 512 | as shipped |
| [mallman/CoreMLaMa](https://github.com/mallman/CoreMLaMa) | original (IOPaint) | Apache-2.0 | builds `LaMa.mlpackage`; a prebuilt one is [Jia-Liu/big-lama-coreml](https://huggingface.co/Jia-Liu/big-lama-coreml), 94,867,388 bytes zipped | Core ML, fp32, CPU and GPU | **fixed 800 x 800** ("Flexible input shapes are not (currently) supported") | Core ML conversion |
| [Vincent615/lama-2048-onnx](https://huggingface.co/Vincent615/lama-2048-onnx) | unknown | none stated | fixed 1024 and 2048 files | ONNX | fixed | unknown |

Notes on the table:

- The ogkalu card says it is `FourierUnitJIT` "with improvements that allow inference on images
  with varying input sizes". Measured: 10,239 nodes, no `DFT`, no `Einsum`, 432 `MatMul`,
  inputs `image` and `mask`, output `inpainted`. It returns the known pixels unchanged.
- The smile-forge card (published 17 October 2026) explains the matrix-product rewrite
  (`fourier_matmul.py`): ONNX Runtime's CUDA provider has no `DFT` kernel, and its CPU `DFT` is
  an O(n²) loop for lengths that are not powers of two ("134 s for a 1800x1200 picture"). It
  states agreement with `torch.fft` to 4e-6 and with the original generator to 8e-5. It says
  "ONNX Runtime 1.30 or later", but the file loaded and ran on 1.28.0 here.
- Vincent615 has no card and no licence and is excluded. Liiesl is lighter, not bigger; the
  [mlx-community/LaMa-fp32](https://huggingface.co/mlx-community/LaMa-fp32) card warns that
  bf16 and fp16 compute break LaMa (fp16 "collapses the FFC outright"), so only weight-only
  quantisation with fp32 compute is safe.

### FFT operator support

| Provider | ONNX `DFT` | Source |
|---|---|---|
| CPU | opset 17-19 and 20+, float and double | [OperatorKernels.md](https://github.com/microsoft/onnxruntime/blob/main/docs/OperatorKernels.md) |
| DirectML | opset 20+, float and float16 | same |
| CUDA | none (only the `com.microsoft` `Rfft` and `Irfft` contrib ops) | same |
| WebGPU | opset 17-19 and 20 | [webgpu_execution_provider.cc](https://github.com/microsoft/onnxruntime/blob/main/onnxruntime/core/providers/webgpu/webgpu_execution_provider.cc) |
| CoreML | none | op_builder_factory.cc, linked above |

Every working export found avoids `DFT` by writing the transform as matrix products. So the FFT
export problem is solved in practice: the graph runs on any provider that has `MatMul`, `Cos` and
`Sin`. The cost is an O(n²) product instead of an FFT, at one eighth of the input side (after the
generator's three downsamplings), so at 4096 px the matrices are at most 512 x 512.

## 3. Higher resolution in ONNX Runtime

Yes. Two dynamic exports exist and both run on ONNX Runtime 1.28.0. The app's `ort` crate
(`=2.0.0-rc.13`) takes tensors of any shape, so the Rust side only needs to build the input at
the crop's own size, padded to a multiple of 8.

Measured on CPU, Apple M5, 10 cores, 32 GB, 2 intra-op threads, a synthetic image with a square
hole half the side, best of 3 runs after one warm-up. Other sessions loaded the machine (load
average 12 to 52), so absolute times are inflated: the same fixed file took 6,640 ms here
against 2,540 ms in `accel.rs`. Compare rows with each other, not with `accel.rs`.

| File | Side | Run | Peak RSS |
|---|---|---|---|
| `lama-manga.onnx` (fixed) | 512 | 6,640 ms | 1.08 GiB |
| ogkalu dynamic | 512 | 5,243 ms | 0.94 GiB |
| smile-forge dynamic | 512 | 4,357 ms | 1.16 GiB |
| ogkalu dynamic | 1024 | 13,540 ms | 2.50 GiB |
| smile-forge dynamic | 1024 | 10,447 ms | 2.54 GiB |
| ogkalu dynamic | 1536 | 20,841 ms | 5.14 GiB |
| smile-forge dynamic | 1536 | 32,091 ms | 7.07 GiB |
| ogkalu dynamic | 2048 | 93,883 ms | 7.95 GiB |
| smile-forge dynamic | 2048 | 43,999 ms | 9.52 GiB |

The load changed during the run, so the 1536 and 2048 times are the least reliable rows.

What the numbers say:

- Time grows a little slower than pixel count (4x the pixels at 1024 cost about 2.6x the time).
  A 1024 px region tiled at 512 with 128 px overlap is 9 runs today; one dynamic run is about
  2.6 runs' worth. Wide regions get faster, not slower.
- Memory grows with pixel count, about 2 to 3 GiB per megapixel. 1536 px needs 5 to 7 GiB on
  the CPU and 2048 px needs 8 to 9.5 GiB. smile-forge reports 4 GB at 1536 on a GPU and 0.1 s on an RTX PRO 6000.
- The LaMa paper ([arXiv 2109.07161](https://arxiv.org/abs/2109.07161)) trains on 256 px crops
  and reports that the model holds up at higher resolutions than it saw in training. The
  smile-forge card puts the limit at "about two thousand pixels a side". manga-image-translator
  runs the dreMaz finetune with `inpainting_size` 2048 by default (it resizes the long side to
  2048 and pads to a multiple of 8,
  [inpainting_lama_mpe.py](https://github.com/zyddnys/manga-image-translator/blob/main/manga_translator/inpainting/inpainting_lama_mpe.py)).

### Parity problem with the ogkalu export

The ogkalu file is supposed to be the same weights as the shipped one. It does not behave like
it. On a smooth test image with thin diagonal lines and four small holes at 512 px:

| Pair | Mean difference in the hole | Largest |
|---|---|---|
| ogkalu vs shipped | 6.97 levels | 153.7 |
| smile-forge (different weights) vs shipped | 6.66 levels | 106.8 |

Error against the true image in the hole: shipped 5.70, smile-forge 7.34, ogkalu 8.50 levels.
On a hard checkerboard with a 256 px hole: shipped 49.9, smile-forge 47.3, ogkalu 76.4.
`docs/research/manga-inpaint-models.md` (Benchmark 4) found only 1.7 to 2.6 levels between a
TorchScript and the shipped ONNX export of the same checkpoint. So the ogkalu graph is as far
from the shipped one as a different model is. Do not adopt it without a parity check against
the PyTorch checkpoint. If it fails, export our own dynamic graph from
[mayocream/lama-manga](https://huggingface.co/mayocream/lama-manga) (SafeTensors of the same
weights, MIT, 204,332,996 bytes) with the smile-forge `fourier_matmul.py` rewrite (Apache-2.0),
and pin it like the other models.

### Providers

- CPU: works (above).
- WebGPU: the measured winner for the fixed file today. Not tried with dynamic shapes. Shader
  compilation per new shape is a risk; rounding crop sides up to a multiple of 64 bounds the
  number of distinct shapes.
- CoreML: keep off, as now. No `Neg` or `Range` builders, and dynamic shapes are worse there.
- DirectML and CUDA: no `DFT` needed any more, so nothing structural blocks them. Unmeasured.

## 4. Local plan

Recommended shape: one more file and a geometry setting, no new engine family.

1. Produce the artifact: run the parity check on the ogkalu file; if it fails, export our own
   dynamic graph as above and host it.
2. `scripts/fetch-models.sh`: fetch the dynamic file, pinned by revision and SHA-256.
3. `src-tauri/src/weights.rs`, `MODELS`: a new `ModelPackage` (for example id `bigInpainter`,
   `required_by: &["bigLama"]`) so the downloader can offer it.
4. `crates/cleaner-core/src/engines/model.rs`: `MODEL_INPUT`, `TILE_STRIDE`, `TILE_HANDOFF` and
   `MAX_BOX` are constants tied to 512. Make them a per-engine geometry value: fixed 512 for the
   current file; for the dynamic file, a cap (1536 suggested) with tiles of the cap size above
   it and the same 128 px overlap and decline rules.
5. `crates/cleaner-core/src/engines/lama.rs`: build the input tensor at the crop's own size,
   padded to a multiple of 8 with real page pixels or edge replication (the existing rule). The
   input and output names differ per file (`image`/`mask`/`inpainted` for ogkalu,
   `rgb`/`mask`/`inpainted` for smile-forge), so read them from the session.
6. `crates/cleaner-core/src/accel.rs`: a `BIG_LAMA` profile with `uses_dft: false` and
   `measured_peak_rss` per side, so the memory gate can refuse 1536 on small machines. Correct
   `LAMA.uses_dft` at the same time, after re-timing CUDA and DirectML.
7. `crates/cleaner-core/src/registry.rs` and `residency.rs`: register it as an inpainter
   (`Kind` plus `model_name`) and evict it before SAM the way LaMa is evicted
   (`residency.rs:214`).
8. `src-tauri/src/run.rs` and `library.rs`: a stored engine value (`"big-lama"`) and its
   provenance.
9. `src/lib/model/pipelines.js`: set the `big-lama` row's `files` and `ready`. The i18n note
   already exists in every language.

Memory and speed: about 1 GiB at 512, 2.5 GiB at 1024, 5 to 7 GiB at 1536 on the CPU. One run per
region up to the cap. Small regions cost the same as today.

## 5. Cloud plan

**As the Klein pre-fill (do this first).** `LamaFill` in `deploy/cloud/common/flux.py` tiles at
512 because the file is fixed. With the dynamic file it runs a crop up to the cap in one pass and
keeps tiling above it. The graph has no `DFT`, so it places fully on CUDA. Changes:
`manifest.py` (`LAMA_REPO`, `LAMA_REVISION`, `LAMA_FILE`, `LAMA_BYTES`; the seeded volume
re-seeds about 206 MB), `flux.py` (`LAMA_SIDE` becomes a cap, padding to a multiple of 8),
the recipe ids (the fill changes, so the output changes), `tests/test_worker.py` and the Klein v2
paragraph in `docs/cloud-contract.md`.

**As its own cloud engine (later, only if asked).** A new `ProductionModel` with
`runner="lama"`, a one-file snapshot, any GPU (an L4 is plenty: 4 GB at 1536 per smile-forge),
small `worker_memory_gib`, and a `runner_for` entry in `worker.py`. The interface would get a
cloud row in `pipelines.js`, and the Rust cloud wire would need the model id. Value is low: the
desktop CPU already runs it in seconds, so the cloud only helps weak machines, and cold start and
the slow link to us-east-1 would cost more than the compute.

## 6. Quality evidence

- No public benchmark compares manga LaMa, original big-lama, MI-GAN and AOT-GAN on manga text
  removal with numbers.
- The dreMaz card says only that the finetune is "significantly better than older lama_mpe on
  manga", with no figures.
- manga-image-translator defaults to `lama_large` (the dreMaz finetune) at `inpainting_size` 2048
  ([README config schema](https://github.com/zyddnys/manga-image-translator)); its
  `Inpainter.default` key still maps to its AOT model
  ([inpainting/\_\_init\_\_.py](https://github.com/zyddnys/manga-image-translator/blob/main/manga_translator/inpainting/__init__.py)).
  [BallonsTranslator](https://github.com/dmMaze/BallonsTranslator) offers `lama_large_512px`, AOT
  (taken from manga-image-translator) and PatchMatch.
- In this repo, Benchmark 4 of `docs/research/manga-inpaint-models.md`: the manga LaMa kept a
  black balloon black and removed every sound effect, but smeared speed lines and left a light
  streak across screentone where Klein 9B continued the tone. Part of that may be tiling at 512.
- The MI-GAN ([Picsart-AI-Research/MI-GAN](https://github.com/Picsart-AI-Research/MI-GAN)) and
  AOT-GAN ([arXiv 2104.01431](https://arxiv.org/abs/2104.01431)) papers report on Places2 and
  CelebA-HQ, not manga.
- On the two synthetic tests above, the original big-lama weights were no better than the
  shipped manga file at 512. This is a synthetic image, not a page. Section 7 has the real-page
  test.

## 7. Real-page test: level correction vs Big LaMa (19 October 2026)

Question: the manga LaMa is weak on screentone and shifts grey levels. Would a brightness
correction help more than Big LaMa? Does the `uses_dft` fix change anything?

**The `uses_dft` fix does not change the picture.** It only decides which processor runs the
graph (CPU, CUDA, DirectML). The maths is the same on each, so the output is the same within
rounding. It can make LaMa faster on Windows and Linux GPUs. It cannot fix screentone or grey
levels.

**What the app does today.** `lama.rs` writes the model's answer only through the applied mask,
with a 5 px ramp at its edge, and averages the channels on grey pages. It does **not** correct
tone. The generated path already does: `PreparedRender::tone` in `engines/render.rs`
(preprocessing V2) measures the answer's drift on a ring 6 to 18 px outside the applied mask,
with 7 px box means, in 8 level bands, clamped to 24 levels, and corrects the answer before the
blend. FLUX and cloud renders get this. `lama.rs` returns `tone: None`.

**Method.** 111 pages copied from the app library (one in six pages of every chapter, copies
only). The test hides real page areas under text-shaped masks (Hiragino glyphs grown 4 px, as
`fit` grows ink), a big "ドドド" sound effect, or a balloon-sized block, so the true pixels
under the hole are known. It runs the shipped `lama-manga.onnx` on a 512 px crop (ONNX Runtime
1.28.0, CPU), writes the answer through the hole and a 5 px ramp, and compares with the truth.
Corrections are applied to the answer before that write:

- `offset`: one median shift learned on the ring 6 to 24 px outside the hole (9 px box means).
- `bands`: the same ring, 8 level bands, clamped to 24 levels: a Python copy of the V2 idea
  without its background filter.
- `texture`: `bands`, then the fill's fine detail scaled up to the ring's.

Measures, all in 8-bit levels: `tone error` is the 9 px box-mean grey level against the truth
over everything written (what the eye reads as "too dark" or "too light"); `bias` is the signed
mean shift in the hole (negative = darker); `halo` is the pixel error in the 5 px ramp;
`texture` is the fill's fine-detail strength against the truth's (1.0 = dots kept, lower =
smeared).

Small holes over screentone, 27 crops from 111 pages:

| Correction | Tone error | Bias | Halo | Texture |
|---|---|---|---|---|
| none (the app today) | 4.92 | -2.56 | 3.51 | 0.94 |
| offset | 4.01 | -0.31 | 3.23 | 0.94 |
| bands | **3.96** | **-0.14** | **3.22** | 0.94 |
| texture | 4.00 | -0.21 | 3.22 | 1.01 |

`bands` was better than none in 25 of 27 crops; the worst loss was 0.44 levels. By hole type,
tone error went 4.36 to 3.40 (text), 6.18 to 5.17 (sound effect), 5.06 to 4.16 (block). On a
first run with stricter screentone picking (6 crops) and on 10 smooth grey crops and 12 busy art
crops, the order was the same: smooth grey 2.38 to 1.64, art 9.86 to 9.55.

Large sound-effect holes (a 230 px "ドドド", about 700 px wide) on a 1024 px crop, 12 crops
from 6 pages. Tiles are 512 px with 128 px overlap, as `lama.rs` and `LamaFill` do (4 runs per
crop):

| Variant | Tone error | Bias | Texture |
|---|---|---|---|
| shipped LaMa, 512 tiles (the app today) | 6.03 | -2.16 | 0.90 |
| shipped LaMa, 512 tiles + `bands` per tile | **5.57** | **-0.34** | 0.90 |
| manga LaMa, one 1024 pass (ogkalu export) | 5.88 | -1.41 | **0.77** |
| original big-lama, one 1024 pass (smile-forge) | 5.76 | 0.02 | 0.89 |

What this says:

1. **LaMa darkens its fill by about 2 to 2.5 levels**, on every kind of page area. A ring-based
   level correction removes almost all of it and cuts the tone error by 10 to 20%. This is the
   "few grey levels" problem, and it is fixable without a new model.
2. **The correction must use the model's raw answer on the ring.** The shipped graph moves known
   pixels by the same drift it puts in the hole, which is what makes the drift measurable. The
   ogkalu and smile-forge graphs return known pixels unchanged, so the ring shows no drift for
   them and the correction cannot be learned from their output.
3. **Screentone dots themselves survive well at these hole sizes** (texture 0.94 in small holes).
   Scaling the fine detail up (`texture`) did not help the grey level. The visible weakness is
   level and smoothness in wide holes, plus art the hole covers, which no inpainter can know.
4. **One big pass with the manga weights smeared screentone more** than 512 tiles (texture 0.77
   against 0.90) and only matched tiles on level. The parity doubt about that export (section 3)
   still stands, so this is not final, but it gives no reason to build Big LaMa first.
5. The original big-lama weights came out level-neutral (bias 0.02) and slightly better than
   uncorrected tiles, but worse than corrected tiles.

Limits: synthetic holes on real pages, not real lettering; one 512 px crop per small case
instead of the app's exact fitted geometry; the halo is measured with a simple linear ramp.
Pictures and scripts are in the session scratchpad (`tone_exp.py`, `big_exp.py`), not in the
repository.

### Plan for the level correction

1. `crates/cleaner-core/src/engines/lama.rs`: keep each tile's raw answer for the ring around
   the applied mask (the tile output already covers it), learn the curve the way
   `PreparedRender::tone` does, and apply it before `commit_srgb`. The cleanest form reuses
   `render.rs`'s tone code on a region-sized buffer of LaMa's answer, so both paths share one
   correction and one `ToneReport`, and `Rendered::tone` stops being `None` for LaMa.
2. Per tile or per region: per tile matched the test above; per region needs the tiles' answers
   kept until the end (memory is one region's floats, small).
3. `deploy/cloud/common/flux.py` `LamaFill` (the Klein pre-fill) has the same darkening and can
   take the same correction.
4. Tests: a grey-page fixture where the corrected fill's box-mean level matches the page within
   a level, and the existing "a grey page gets a grey patch" test must still pass.

## Open risks

1. The ogkalu export does not match the shipped graph (section 3). Parity against PyTorch must
   come first.
2. On 12 large holes from real pages, one 1024 pass with the manga weights smeared screentone
   more than 512 tiles (section 7). Real lettering, more pages and a parity-checked export are
   still needed before ruling it out.
3. WebGPU behaviour with changing shapes (shader recompiles, memory) is unknown.
4. Memory: 5 to 7 GiB at 1536 on the CPU is too much for an 8 GB machine. The cap must come from the
   memory gate, not a constant.
5. Licence labels disagree: the dreMaz source is MIT, the ONNX repos tag Apache-2.0. Both are
   permissive; record the source licence.
6. smile-forge is two days old and says it needs ONNX Runtime 1.30; it ran on 1.28.0 here on CPU
   only.
7. The `uses_dft` error in `accel.rs` already affects provider choice for the shipped LaMa.

## Follow-ups (not done here)

- Add the ring-based tone correction to `lama.rs` (section 7 plan), then to `LamaFill`.
- Correct `LAMA.uses_dft`, the `accel.rs` comment and `model.rs:4`, then time CUDA, DirectML and
  CoreML again for the shipped file.
- Parity check of the ogkalu file against the PyTorch checkpoint, or our own dynamic export.
- A real-lettering comparison: shipped 512 tiles vs dynamic one pass, after the parity check.
