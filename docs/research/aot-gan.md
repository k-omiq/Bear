# AOT-GAN for coloured manhwa sound effects: research and integration plan

**Decision, 2026-10-19: dropped.** The user decided AOT is not needed after the section 7 bench, where manga LaMa beat it on colour. Nothing below is planned work; the note stays as the record of why.

Research note, 2026-10-19, revised the same day. Nothing here is in the app. The question:
should the app add an AOT inpainter beside manga LaMa (rung 2), locally and in the cloud, and how?

**Target use (set by the user):** coloured manhwa pages, and mostly the small sound effects drawn
over the art. Not black-and-white manga, where LaMa and Klein stay the engines; the screentone
points below are kept only as background.

Short answer: the useful AOT is not the paper's model. It is a small AOT generator trained for
comic text removal by manga-image-translator (5.7M parameters, 22.8 MB). It is a full RGB model
with no greyscale shortcut, but its training data, and so its colour fidelity, is unpublished. It
exports cleanly to a fixed 512 x 512 ONNX graph that runs **entirely on CoreML** (380 of 380 nodes),
about 0.2 to 0.35 s per tile on this Mac, against LaMa's 0.63 s on WebGPU and 1.9 s on CoreML. A
small sound effect fits in one tile, so a strip with dozens of them is where that speed shows.
Nobody has published how it does on colour against LaMa, so it was benched here on one coloured
manhwa page the user supplied (section 7): **manga LaMa beat it on 36 of 44 holes with known truth
and by eye on both real sound effects; AOT turned coloured fills grey on larger holes.** AOT is
about three times faster than LaMa on the CPU and runs in 165 ms on CoreML, and that is all it has
on this page.

## 1. Which AOT

There are two different networks under the name.

### The paper's AOT-GAN (not recommended)

- Paper: Zeng, Fu, Chao, Guo, "Aggregated Contextual Transformations for High-Resolution Image
  Inpainting", arXiv [2104.01431](https://arxiv.org/abs/2104.01431) (April 2021), later IEEE TVCG.
- Code: [researchmm/AOT-GAN-for-Inpainting](https://github.com/researchmm/AOT-GAN-for-Inpainting),
  Apache-2.0 (GitHub license API), last push 2024-05-08.
- Weights: CelebA-HQ and Places2 only, as Google Drive folders linked from the README. No manga or
  anime weights. No published hashes.
- Generator ([src/model/aotgan.py](https://github.com/researchmm/AOT-GAN-for-Inpainting/blob/master/src/model/aotgan.py)):
  256 channels, 8 AOT blocks, dilation rates 1, 2, 4, 8; trained at 512 x 512
  ([src/utils/option.py](https://github.com/researchmm/AOT-GAN-for-Inpainting/blob/master/src/utils/option.py)).
  15.2M parameters, 58 MB float, per [qualcomm/AOT-GAN](https://huggingface.co/qualcomm/AOT-GAN).
- Input contract ([src/test.py](https://github.com/researchmm/AOT-GAN-for-Inpainting/blob/master/src/test.py)):
  RGB scaled to -1..1, mask 0/1 with 1 = hole, the hole filled with **white** (`image * (1 - mask) + mask`),
  output tanh in -1..1.
- Qualcomm AI Hub publishes an ONNX export of the CelebA-HQ model at fixed 512 x 512 (inputs 0..1),
  which shows the paper's graph exports. Faces and scenes are not manga; skip it.

### The comic AOT from manga-image-translator (the candidate)

zyddnys trained their own generator for manga text removal and named it AOT because it reuses the
AOT block. It is a different network from the paper's:

- Code: [manga_translator/inpainting/inpainting_aot.py](https://github.com/zyddnys/manga-image-translator/blob/main/manga_translator/inpainting/inpainting_aot.py)
  (`AOTGenerator`). Head and tail are gated convolutions with scaled weight standardisation;
  the body is 10 AOT blocks at 128 channels, dilation rates 2, 4, 8, 16; base width 32;
  two stride-2 downsamples and two transposed-conv upsamples.
- Parameters: **5,679,244** (counted from the code here), 22.7 MB float32.
- Compute: 200.8 GFLOP at 512 x 512, 803 GFLOP at 1024 x 1024 (torch flop counter, here).
- Training: the repo's [training/inpainting/README.md](https://github.com/zyddnys/manga-image-translator/blob/main/training/inpainting/README.md)
  points only to [zyddnys/anime-inpainting-deq](https://github.com/zyddnys/anime-inpainting-deq), a
  "work in progress" repo described as "Anime image inpainting", with no licence, no data list and a
  different model. **The training data for this checkpoint is not published**, so whether it saw
  colour pages, and how many, is not known. The repo's issues and READMEs (English and Chinese)
  say nothing more.

Weights, all the same checkpoint (SHA-256 `878d541c68648969bc1b042a6e997f3a58e49b6c07c5636ad55130736977149f`,
published in the upstream code and in the Hugging Face LFS metadata; not downloaded or re-hashed here):

| File | Where | Bytes | Licence as stated |
|---|---|---|---|
| `inpainting.ckpt` | [manga-image-translator release beta-0.3](https://github.com/zyddnys/manga-image-translator/releases/download/beta-0.3/inpainting.ckpt) (release dated 2022-04-23) | 22,785,303 | repo is GPL-3.0 |
| `aot_inpainter.ckpt` | [dreMaz/mit_models](https://huggingface.co/dreMaz/mit_models/resolve/main/aot_inpainter.ckpt) (BallonsTranslator's download) | 22,785,303 | GPL-3.0 LICENSE file in the repo |
| `inpainting.ckpt` | [ogkalu/aot-inpainting](https://huggingface.co/ogkalu/aot-inpainting) | 22,785,303 | card says MIT |

Conversions of it:

| File | Where | Bytes | SHA-256 (HF LFS) | Notes |
|---|---|---|---|---|
| `model.safetensors` | [mayocream/aot-inpainting](https://huggingface.co/mayocream/aot-inpainting) @ `cffe2346ac2b` | 22,732,864 | `1b4fea17a84a228c2097a42ab2f403357f07bb56ae022dc243b40817b7aa87d1` | Koharu's; no pickle; config says pad to 8, max side 1024 |
| `aot.onnx` | [ogkalu/aot-inpainting](https://huggingface.co/ogkalu/aot-inpainting) @ `42ffc84ff1bd` | 23,068,213 | `ffd39ed8e2a275869d3b49180d030f0d8b8b9c2c20ed0e099ecd207201f0eada` | comic-translate's ONNX |
| `aot_traced.pt` | same repo | 23,141,682 | `552d9ad440258fa14907fc2492cf172c51983760c4619861f8b7b362a762af0b` | TorchScript |
| `inpainting_aot.onnx` | [Liiesl/aot-inpainting-onnx](https://huggingface.co/Liiesl/aot-inpainting-onnx) | 23,066,382 | `ced9b2171953b0f7486336a4663bf5086e9c1f2eb130330776466f34806c3bb2` | no card |

The "MIT" on the conversions is the converter's claim; a conversion cannot change the upstream
terms. The app itself is GPL-3.0-only (README "Licence"), and it already downloads
`comictextdetector.onnx` from the same manga-image-translator release line, so GPL-3.0 weights are
not a new kind of problem here.

## 2. Model contract (the comic AOT)

Taken from the three tools that run it, which agree:
[manga-image-translator](https://github.com/zyddnys/manga-image-translator/blob/main/manga_translator/inpainting/inpainting_lama_mpe.py) (`LamaMPEInpainter._infer`, which `AotInpainter` inherits),
[BallonsTranslator](https://github.com/dmMaze/BallonsTranslator/blob/dev/ballontranslator/modules/inpaint/inpaint_default.py) (`AOTInpainter`),
[comic-translate](https://github.com/ogkalu2/comic-translate/blob/main/modules/inpainting/aot.py) (`AOT.forward`).

- Inputs: `image` float32 `[N, 3, H, W]` RGB in **-1..1** (`x / 127.5 - 1`); `mask` float32
  `[N, 1, H, W]`, binarised at 0.5, **1 = hole**. Inside the network the mask is concatenated
  first (`cat([mask, img])`), which only matters to whoever writes the export.
- The caller zeroes the hole: `image *= (1 - mask)`. In -1..1 that is mid grey, not white as in the
  paper's code. The graph does **not** do this itself (unlike `lama-manga.onnx`, see `lama.rs`).
- Output: `[N, 3, H, W]` in -1..1, clipped inside the network in eval mode. Convert with
  `(y + 1) * 127.5`. It returns a whole image; every tool composites it through the mask.
- Shape: fully convolutional, so any size. H and W should be multiples of 4: a 510 x 514 input came
  back 508 x 512 here. The tools pad or resize to multiples of 8 and enforce a 128 px minimum.
  Working size in the tools: longest side capped at 1024 (manga-image-translator's code default,
  comic-translate) or 2048 (BallonsTranslator, Koharu `inference_with_max_side(.., 2048)`).
- BallonsTranslator runs it per text block on a window 1.7 times the block's box
  (`inpaint/base.py`, `enlarge_window(..., ratio=1.7)`), which is close to how this app crops.
- One property to respect when tiling: each AOT block's gate normalises by the mean and standard
  deviation over the whole feature map (`my_layer_norm`). So a tile's answer depends on everything in
  the tile, including padding. The app's tiling already feeds real page context, which is right.

### Colour: what is known

- The network is RGB in, RGB out, three independent channels end to end (`in_ch = 4` is mask plus
  RGB, `out_ch = 3`). There is no luminance-only path and no colour-space step, so it can return
  colour; nothing in the code forces grey.
- All three tools feed it colour pages unchanged. comic-translate lists AOT as one of its two
  inpainters and names Korean among its source languages and Naver webtoons among its examples
  ([README](https://github.com/ogkalu2/comic-translate/blob/main/README.md)). That shows people run
  it on coloured webtoons; it is not a measurement of how well.
- What is **not** known: the share of colour pages in training, and whether it keeps hue across a
  hole. The training repo's "anime image" description hints at colour illustrations, nothing more.
- Its output is 8-bit after `(y + 1) * 127.5`, the same depth as LaMa's. The app's tone check in
  `engines/render.rs` (ring-based level alignment, which a parallel session is adding to LaMa's
  path, `docs/research/big-lama.md` section 7) would apply to AOT too if it shares rung 2's loop.

## 3. ONNX export: tried here

Measured on this Mac (Apple M5, 32 GB), ONNX Runtime 1.31.0 (Python), torch 2.13.0, with the manga
AOT architecture and **random weights** (the checkpoint was not downloaded). Speed, op support and
CPU/CoreML agreement do not depend on the weight values; image quality does and was not measured.
The machine had a load average near 10 from other sessions, so treat times as rough; the CoreML
versus CPU gap is large enough to survive that. Scripts and logs are in this session's scratchpad,
not in the repo.

**Export works.** Both `torch.onnx.export(dynamo=False)` (opset 17) and the dynamo exporter
(opset 18) produce a graph that matches PyTorch to 0.0000 on the CPU provider. Two export steps
matter:

- Fold the weight standardisation into plain conv weights before export (`get_weight()` once, then
  a plain `conv2d`). Exact (max difference 0.0) and it removes the per-call weight arithmetic.
- Write `my_layer_norm`'s unbiased standard deviation out by hand, so a dynamic graph does not carry
  a `Shape`/`ReduceProd` chain. The dynamo exporter then gives a clean graph: 389 nodes, 72 `Conv`,
  66 `Pad` (reflect), 4 `ConvTranspose`, 10 `ReduceMean`.

Ops: `Conv` with dilation up to 16, `ConvTranspose`, `Pad` mode reflect, `Sigmoid`, `Relu`,
`ReduceMean`, `Sqrt`, `Pow`, elementwise. **No `DFT`**, which is the op that makes LaMa slow on
CoreML (`accel.rs`, `LAMA` profile note). Nothing in it should trouble DirectML or CUDA, but neither
was run.

512 x 512 tile, one image, median of 5 warm runs, separate process per row:

| Graph | Provider | Build | Run | Peak RSS | Notes |
|---|---|---|---|---|---|
| `lama-manga.onnx` (shipping) | CPU, 2 threads | 4.9 s | 2,696 ms | 874 MiB | for reference |
| AOT, dynamic H/W | CPU, 2 threads | 0.1 s | 909 to 1,820 ms | 526 MiB | spread is machine load |
| AOT, dynamic H/W, 1024 x 1024 | CPU, 2 threads | 0.0 s | 4,958 ms | 1,860 MiB | |
| AOT, **fixed 512** | CPU, 2 threads, default optimisation | 0.0 s | **9,964 to 19,812 ms** | 1,087 MiB | see below |
| AOT, fixed 512 | CPU, 2 threads, `ORT_ENABLE_EXTENDED` | | 1,720 ms | | |
| AOT, **fixed 512** | **CoreML MLProgram** | 0.5 to 0.9 s | **211 to 347 ms** | 176 to 188 MiB | 1 partition, 380 of 380 nodes |
| AOT, fixed 512 | CoreML NeuralNetwork | 21.6 s | 854 ms | 2,705 MiB | 29 partitions |
| AOT, dynamic H/W (dynamo) | CoreML MLProgram | fails | | | `HandleNegativeAxis ... axis 4 is not in valid range [-4,3]` |
| AOT, dynamic H/W (legacy) | CoreML MLProgram | 3.9 s | 2,128 ms | | 27 partitions |
| AOT, dynamic H/W | CoreML NeuralNetwork | 0.6 s | 1,029 to 1,938 ms | 4,712 MiB | up to 405 partitions |
| `lama-manga.onnx` | CoreML MLProgram | fails | | | "Failed to build the model execution plan" |

The app's own LaMa numbers (`docs/findings.md`, "Engines and quality"): 629 ms WebGPU, 1,486 ms CPU,
1,856 ms warm CoreML, 2.1 s session build.

Findings from the table:

1. **CoreML wants the fixed 512 graph in MLProgram format.** Then the whole model runs inside
   CoreML, about 2 to 3 times faster than LaMa on WebGPU and 5 to 9 times faster than LaMa on CoreML.
   CoreML against the CPU provider: max difference 0.0000 in MLProgram, 0.025 (about 3 grey levels)
   in NeuralNetwork. The app builds CoreML with `ort::ep::coreml::CoreML::default()` (`accel.rs`),
   which is NeuralNetwork; ort 2.0.0-rc.13 has `.with_model_format(ModelFormat::MLProgram)` and
   `.with_model_cache_dir(..)`.
2. **The fixed graph is 10 to 20 times slower on the CPU provider at the default optimisation
   level.** `ORT_ENABLE_ALL` (ONNX Runtime's default, which adds the NCHWc layout pass) is the cause:
   at `DISABLE_ALL`, `BASIC` or `EXTENDED` the same file runs in 1.7 s. The dynamic graph does not
   hit this. So either ship one fixed graph and open it at `Level2` (extended) on non-CoreML
   providers, or ship a dynamic graph for CPU and a fixed one for CoreML. One file plus a per-profile
   optimisation level is simpler.
3. **AOT on CPU is about 1.5 to 3 times faster than LaMa on CPU** at 2 threads, and holds about
   350 MiB less.
4. Dynamic shapes and CoreML do not mix today (an ONNX Runtime CoreML EP bug in one exporter's graph,
   heavy partitioning in the other). The app's tiling is already fixed 512, so this costs nothing.

Existing exports: comic-translate's `aot.onnx` is fed any size up to 1024 by its code, so its axes
are dynamic (read from the code; the file was not opened). Our own export is better for this app:
fixed 512, our input names, weight standardisation folded, made from the safetensors (no pickle).

## 4. Local plan (Rust, ONNX Runtime)

AOT fits rung 2's shape exactly: a held session, fixed 512 tiles with 128 px overlap, the same
decline rules, the same composite. A small manhwa sound effect is one tile, so most regions cost
one model run. What differs is only the tensor contract (-1..1, caller zeroes,
output -1..1) and the provider policy. So it should be a second implementation inside rung 2's
tile loop, not a new rung. MI-GAN was removed because it sat *below* LaMa as an escalation step
nothing started on (`engines/mod.rs`, `docs/findings.md`); AOT only earns a place as the engine a
colour page starts on, chosen by the user or by the page's colour mode, not as a fallback after
LaMa.

Steps:

1. **Export script** (new, for example `scripts/export-aot.py`): load
   `mayocream/aot-inpainting` `model.safetensors` at a pinned revision into the upstream
   `AOTGenerator`, check it against `inpainting.ckpt` loaded with `weights_only=True` once, fold
   weight standardisation, write fixed `[1,3,512,512]` / `[1,1,512,512]` inputs named `image` and
   `mask`, opset 17, output `output`. Publish the file (a Hugging Face repo of ours) with its SHA-256.
   Verify the exported file against PyTorch on a real page.
2. **`scripts/fetch-models.sh`, `src-tauri/src/weights.rs`**: add `aot-manga.onnx` (about 23 MB)
   with its SHA-256 and `required_by` for the new engine id. README model table gets a row
   (GPL-3.0, upstream manga-image-translator).
3. **`crates/cleaner-core/src/engines/lama.rs`**: lift the per-tile tensor fill and read-back into a
   small contract (value range, whether the caller zeroes, output range) so the tile loop is shared.
   Today the loop builds 0..1 tensors inline. Option: a new `engines/aot.rs` holding only the AOT
   contract and its `RESIDENT_BYTES`, calling the shared loop from `engines/model.rs`. Check the
   hand-off rule (hole cut at `done`, not at `commit`) still holds for AOT with the existing noise
   test.
4. **`crates/cleaner-core/src/accel.rs`**: a new `AOT` `ModelProfile`: `uses_dft: false`,
   `measured_better: &[CoreMl]` on macOS, CPU elsewhere until DirectML and CUDA are timed. Two
   additions the profile does not have yet: CoreML model format (MLProgram, plus a cache directory
   so the build is paid once) and graph optimisation level (extended, because of finding 2). Add to
   `PROFILES`. Its peak RSS rows come from a run through the app, not from this note.
5. **`crates/cleaner-core/src/registry.rs`**: `Kind::Inpainter` can cover both, with the display name
   ("AOT Manga") passed to `register_named` as LaMa does; `RESIDENT_BYTES` measured through the app
   (about 190 MiB on CoreML and 530 MiB on CPU process RSS here, against LaMa's 510 MB constant).
6. **`crates/cleaner-core/src/residency.rs`**: the cached-session key must name the model, so a LaMa
   session and an AOT session are never confused on checkout. `ONNX_IDLE_GRACE` (300 s) applies as is.
7. **`crates/cleaner-core/src/patch.rs`** (`Engine`), **`src-tauri/src/run.rs`** (`EnginePick`,
   `open_inpainter`), **`src-tauri/src/region.rs`**: an engine choice and provenance value. Keep
   the stored provenance readable by older builds the way `migan` was handled. If the bench says
   so, route colour pages (the page's `ColorMode`, `crates/cleaner-core/src/image/mod.rs`) to AOT by
   default and grey pages to LaMa.
8. UI: `src/lib/model/pipelines.js`, `model-names.js`, the settings and first-launch dialogs and
   their tests list models by name today; add AOT there.
9. Tests: the four measured contract properties `lama.rs` pins for LaMa (range, polarity, channel
   order, graph zeroing) re-measured for the AOT file; a skip-if-missing test like LaMa's.

Memory and speed expected: about 0.2 to 0.35 s per 512 tile on CoreML (MLProgram) and 0.9 to 1.8 s
on CPU at 2 threads; process RSS for the session 180 to 530 MiB depending on provider; download
23 MB against LaMa's 207 MB.

## 5. Cloud plan (Modal worker)

The cloud already runs ONNX inpainting on GPU: `LamaFill` in `deploy/cloud/common/flux.py` fills
the hole before Klein, with ONNX Runtime on CUDA, fixed 512 tiles, 128 px overlap. AOT can reuse
that pattern almost line for line.

Two possible uses, in order of value:

1. **As the pre-fill for Klein** (where LaMa is now). Only worth it if the bench shows AOT's fill
   gives Klein a cleaner start. Change: make `LamaFill` take the contract (range in and out, who
   zeroes) and a model path, add `AOT_REPO`, `AOT_REVISION`, `AOT_FILE`, `AOT_BYTES` to
   `deploy/cloud/common/manifest.py` beside `LAMA_*`, seed it into `<snapshot>/adapters`, and bump
   the recipe id (`mc-flux2-klein-inpaint-v3`) because the output changes. Tests in
   `deploy/cloud/tests/test_worker.py` already check the LaMa path and provider.
2. **As its own cloud recipe** (a plain AOT render). Little reason: the model is small enough that
   any local machine runs it in about a second per tile, so the cloud adds only upload and download
   time. If wanted anyway: a recipe id such as `mc-aot-manga-v1` in `PINNED_RECIPES`, a runner in
   `runner_for` (`worker.py`) that wraps the generalised fill and nothing else, on a CPU container
   or the smallest GPU on the allowlist.

ONNX or PyTorch: ONNX, so local and cloud run the same file (the same reason `LamaFill` is ONNX).
The dynamic graph is fine on CUDA if larger crops are wanted, but the fixed file keeps parity with
local. GPU need: tiny. 5.7M parameters, about 200 GFLOP per 512 tile, well under 1 GB of VRAM;
it never sets the GPU size. CUDA and fp16 were not tested here (manga-image-translator warns that
its LaMa darkens in fp16; AOT under fp16 is unknown, so keep fp32).

## 6. Quality on coloured manhwa, and the bench

### What exists in public

**No public comparison of AOT against LaMa or MI-GAN was found, on colour or on black and
white.** Searches covered manga-image-translator, BallonsTranslator, comic-translate and Koharu
issues and READMEs, and general search.

Indirect signals only:

- In manga-image-translator the enum value `default` still maps to `AotInpainter`
  ([inpainting/__init__.py](https://github.com/zyddnys/manga-image-translator/blob/main/manga_translator/inpainting/__init__.py)),
  but `InpainterConfig` defaults to `lama_large`
  ([config.py](https://github.com/zyddnys/manga-image-translator/blob/main/manga_translator/config.py)),
  and its README recommends `Inpainter: lama_large`. `lama_large` is
  `dreMaz/AnimeMangaInpainting` `lama_large_512px.ckpt`, the same checkpoint as our `lama-manga.onnx`.
  That tool is aimed at Japanese manga first.
- comic-translate offers LaMa and AOT (`app/ui/settings/settings_ui.py`: `['LaMa', 'AOT']`, LaMa
  first), and targets webtoons as well as manga. BallonsTranslator and Koharu ship both too.
- The paper reports AOT-GAN on Places2 and CelebA-HQ (colour photos), not against LaMa and not on
  comics; it says nothing about this checkpoint.

### What the app's engines do on colour today (from this repo's notes)

- **LaMa (rung 2):** the only colour evidence is six old cloud crops replayed on CPU LaMa: it kept
  pixels outside the support but "retained yellow lettering on d27 and changed its colour heavily"
  (`docs/findings.md`, mask coverage section). It also shifts levels on grey pages, which the tone
  correction now being added to `lama.rs` targets (`docs/research/big-lama.md`).
- **Klein (rung 3a and cloud):** never run on a colour page. `docs/findings.md` lists "A colour
  page, under the new prompt or the hole" as not run, and `docs/research/manga-inpaint-models.md`
  ends with "Colour pages: nothing here was run on one". On the coloured d27 title the cloud 2.0.0
  recipe still left a ghost (texture 1.91 against source 17.14).
- **Qwen 2511 (cloud):** the opposite failure, on black and white: it painted peach and orange
  blobs onto a grey page (`manga-inpaint-models.md`, Benchmark 1). On colour it was not measured.

So no engine in the app has a measured colour result. AOT would be compared against a blank, not
against a known-good baseline; the bench below measures all of them on the same holes.

### Coloured manhwa on this Mac (read only, bench on copies)

Found by a read-only scan of the library index and the usual comic folders (share of pixels with
channel spread over 20 levels, and a contact sheet by eye). The fixtures in `fixtures/pages` are all
black and white (0.00), and so are most library chapters.

| Pages | Path | Count, size | Why |
|---|---|---|---|
| Surviving the Apocalypse 107 to 109 (Korean, colour) | `/Users/caved/Documents/MT-M5Check/surviving-the-apocalypse-2/{107,108,109}/raws/` | 45 + 50 + 45 strips, 690 x 1600 | many small hand-drawn sound effects over dark, coloured art (역, 와악, 치지지, 철컹); `chapter.json` in the sibling `MangaTypesetter` copy tags 26 lines `sfx` across the three |
| same chapters, second copy | `/Users/caved/Documents/MangaTypesetter/surviving-the-apocalypse-2/{107,108,109}/raws/` | same | already a copy; still copy again before use |
| Korean colour webtoon, one chapter | `/Users/caved/Downloads/images/images 1/` (`01.png` to `67.png`) | 66 strips, 720 x 2500 | bright, flat-colour backgrounds with small sound effects (휘이이, 지동); adult content on some strips |

The library's `apocalypse` project (`p26`, chapters 107 to 109) points at
`/Users/caved/Downloads/apocalypse/`, which no longer exists; its `.mtclean.d` folders hold the old
detection masks and ink only. `docs/findings.md` records 26 sound effects in apocalypse 109 from
the headless Detect run, 7 of them single-stroke Korean that the gate holds.

### The bench

Copy the two chapter sets into the scratchpad first; never open the originals for writing.

1. **Holes.** Run the headless real Detect (the memory note "Headless real run") with SAM-TS-L
   on the copied Apocalypse 109 and the webtoon chapter, and keep the sound-effect masks the app
   would write (`MASKS_AS=detect` padding, as Benchmark 3 did). Expect a few dozen holes, most well
   under 512 px.
2. **Ground truth for colour.** Real sound effects have no clean truth under them. So also move
   each real sound-effect mask onto a text-free patch of coloured art on the same strip (sky,
   skin, gradient, dark background with highlights) and inpaint there, where the truth is the
   original pixels. Twenty to forty such holes per chapter.
3. **Engines, same holes:** AOT (exported fixed 512 graph, CPU is enough), manga LaMa as shipped,
   manga LaMa with the new ring tone correction if it has landed, Klein 4B local (mflux) for a
   quality ceiling. Klein 9B in the cloud only if the local results are close, and with a cost
   estimate first (memory note "Modal bench cost discipline").
4. **Measures** on the moved holes, against truth: mean colour error in CIE Lab (Delta E 2000) over
   the hole, the hue and chroma shift of the fill's mean against the truth's mean (catches LaMa's
   "changed its colour" and any grey drift), the step across the hole edge (halo), and texture
   (local standard deviation ratio) as in `big-lama.md`. On the real sound effects: `ink_left` and
   `out_diff` as in `manga-inpaint-models.md`, plus contact sheets by eye. Time per hole per engine.
5. **Pass line for AOT:** lower colour error and no larger hue shift than LaMa on the moved holes,
   no more lettering left than LaMa on the real ones, and no new artefact class (colour blobs,
   grey patches, ghosts) by eye. Speed is already known to favour AOT.

Cost: a few hours on this Mac, no cloud spend unless step 3's optional Klein 9B run is taken.

Background, for black-and-white pages only: AOT has no Fourier layers, so it is expected to carry
periodic screentone across a wide hole worse than LaMa. That is not the target use and is not
part of this bench.

## 7. Bench: one coloured manhwa page (2026-10-19)

The user supplied one page to bench on: a Korean colour webtoon strip, 690 x 1600, with two
hand-drawn sound effects, 흠칫 (black with a white outline, crossing a panel border onto white
paper) and 두리번 (purple with a white outline, over a grey-green background and a panel edge).
Everything ran on this Mac on copies in the session scratchpad
(`scratchpad/bench/`: `page.png`, `results.json`, `sheet_real.png`, `sheet_moved.png`,
`moved_holes.png`, `page_{lama,aot,telea}.png`; scripts in `scratchpad/scripts/`). Nothing in the
repo ran or changed.

**Setup.**

- AOT weights: `mayocream/aot-inpainting` `model.safetensors` at `cffe2346ac2b`, downloaded with the
  user's approval; SHA-256 matched the Hugging Face record (`1b4fea17...87d1`). Exported to a fixed
  512 graph as section 3 describes (weight standardisation folded, unbiased std written out, dynamo,
  opset 18). The ONNX output matched PyTorch to 4.5e-5 on a -1..1 scale. SHA-256 of the export:
  `4e1f6809907a521818a000974edff6cc491083f41e7dd6691ff3c1ad452684f0`.
- LaMa: the shipped `models/lama-manga.onnx`. Baseline: OpenCV Telea (radius 3).
- Holes: the app's SAM-TS-L text model (`models/sam-ts-l`, CPU) found the glyphs of both sound
  effects (and the balloon text, left out). Each hole is the glyphs plus their white outline (near
  white, min channel 200 or more, within 10 px) grown by 5 px, the app's `MODEL_HOLE_MARGIN`. This
  approximates `fit::outlined` and the fit's growth; it does not stop at strong edges, as the app
  can. Sizes: 흠칫 24,670 px, 두리번 17,572 px.
- Each engine got a 512 x 512 tile of the real page around the hole, its own input contract, and a
  hard composite through the hole (no alpha ramp). CPU provider, 2 threads, extended graph
  optimisation for both models.
- **Truth test ("moved holes").** The real sound effects have no clean truth under them. So the
  two whole-effect hole shapes and the seven single-glyph hole shapes were each pasted at up to 5
  random places on coloured art inside the panels (not over text, not mostly blank paper), and each
  engine filled art whose true pixels are known. 44 holes: 9 whole-effect, 35 single-glyph.
  Scores over the hole, in CIE Lab: mean Delta E 1976 (`dE`), the Delta E between the fill's mean
  colour and the truth's (`mean shift`), mean chroma difference, lightness difference, texture
  (standard deviation of L, fill over truth), and RGB PSNR.

**Results on the 44 moved holes** (means; lower `dE` is better):

| Engine | dE | mean shift | chroma shift | L shift | texture | PSNR | best dE on |
|---|---|---|---|---|---|---|---|
| Telea | 13.08 | 3.96 | -0.24 | +1.29 | 0.93 | 14.9 | 3 |
| **manga LaMa** | **9.99** | 3.03 | +0.54 | -0.09 | 0.94 | **17.5** | **36** |
| AOT | 11.25 | 2.94 | -0.38 | +0.22 | 0.89 | 16.9 | 5 |

Split by size: single-glyph holes LaMa 9.23 against AOT 10.43 (LaMa best on 29 of 35); whole-effect
holes 12.95 against 14.45 (LaMa best on 7 of 9).

By eye (`sheet_moved.png`, the 4 holes best and 4 worst for AOT against LaMa): LaMa redraws
plausible structure, hair strands, an eye, a jacket seam, and keeps the hue; on a face it invents
features that were not there. AOT fills with smooth, creased grey-brown and loses colour on the
larger holes: brown hair and skin come back grey. Its slightly lower `mean shift` comes from not
committing to anything, not from better colour. Telea smears radially, as expected.

**The two real sound effects** (`sheet_real.png`): both models removed all the lettering and the
outline; neither left purple pixels of 두리번 (4,133 in the source hole; AOT 0, LaMa 0) or dark
strokes of 흠칫 over the paper. LaMa's result is clean: white paper above the panel border, the
border redrawn, the background continued. AOT left a pale grey smudge on the white paper above the
border under 흠칫, a faint grey shadow above 두리번, and redrew the border more weakly. LaMa is better
on both.

**Speed and CoreML** (512 tile, median, machine shared with other sessions): AOT 1.89 s on CPU,
LaMa 5.65 s on CPU, Telea 32 ms. AOT on CoreML MLProgram 165 ms, and with the real weights it
differed from the CPU provider by at most 1 grey level (mean 0.00006) inside the holes, which
closes the CoreML parity risk in section 3.

**What this does not cover.** One page, two sound effects, mostly dark muted art: no bright
flat-colour webtoon, no gradient sky, no small single-stroke effects under 50 px. The holes
approximate the app's fit. No ramp, no tone correction (the new ring correction for LaMa would
only help LaMa). Klein was not run: no local FLUX weights since the 2026-10-09 clean-up, and a cloud
run costs money.

## Open risks

- **On the one page benched, AOT is "faster and worse" on colour**, which is MI-GAN's story. The
  training data is unpublished, so there is no reason to expect a different page to reverse it.
- The coloured manhwa found here is two chapters of one series plus one other webtoon, mostly dark
  art. A bench on them is a small sample, like the one black-and-white page the Klein recipe was
  chosen on.
- Training data and terms for the checkpoint are unpublished; the weights travel under GPL-3.0 in
  their origin repo. Compatible with this GPL-3.0 app; not with a future licence change.
- CoreML path depends on MLProgram, which the app does not use yet; the app's CoreML builder would
  need a per-profile format. The fixed graph also needs a per-profile optimisation level on CPU,
  or it is 10 to 20 times slower.
- DirectML, CUDA and WebGPU were not run. WebGPU is what makes LaMa fast on this Mac today; AOT on
  WebGPU is untested.
- Section 3's numbers were taken with random weights on a loaded machine. Real-weight CoreML parity
  was then checked on the section 7 page: at most 1 grey level from the CPU provider.
- The per-feature-map normalisation means tiles near a page edge see edge-replicated padding in
  their statistics; probably harmless with the app's real-context tiles, not measured. On a long
  strip a small sound effect's tile may be mostly one flat colour, which skews those statistics.
- The background for this task said the app ships MI-GAN; it does not any more (removed, see
  `engines/mod.rs` and `docs/findings.md`).

## Go / no-go

**No-go for now.** On the user's coloured manhwa page, manga LaMa beat AOT on 36 of 44 holes with
known truth and by eye on both real sound effects; AOT only wins on speed (165 ms on CoreML), and
LaMa already removes small sound effects cleanly. Revisit only if a bright, flat-colour webtoon
bench (section 6 list) shows AOT keeping colour where LaMa does not.
