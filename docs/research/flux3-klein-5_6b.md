# FLUX 3 Image Klein (5.6B) for manga text removal: research

Research only, 2026-10-19, four days after the model came out. Nothing here was run. Speed and
memory figures come from the publishers' own pages unless a line says "estimate".

## 1. Does it exist

Yes. The official name is **FLUX 3 Image Klein** (BFL also writes "FLUX.3 [klein]").

| Fact | Value | Source |
|---|---|---|
| Publisher | Black Forest Labs | [model card](https://huggingface.co/black-forest-labs/flux-3-image-klein) |
| Release | 15 October 2026 (repo created 13 October, made public on the 15th) | [Diffusers blog](https://huggingface.co/blog/flux-3), Hub API |
| Distilled repo | `black-forest-labs/flux-3-image-klein`, revision `337337621b8526e120f2275d250242f4a6bedd75` (2026-10-16) | Hub API |
| Base repo | `black-forest-labs/flux-3-image-klein-base` (50 steps, CFG, for fine-tuning) | [GitHub](https://github.com/black-forest-labs/flux-3-image-klein) |
| ComfyUI files | `black-forest-labs/flux-3-image-klein-comfy` (bf16, int8 ConvRot, NVFP4) | Hub API |
| Prompt upsampler | `black-forest-labs/flux-3-image-klein-pup`, 9B, fine-tuned from Qwen3.5-9B, Apache-2.0, 18.8 GB | [GitHub](https://github.com/black-forest-labs/flux-3-image-klein) |
| Gated | Yes, `gated: auto` (click to accept, needs a Hugging Face login and token) | Hub API |
| Parameters | 5.6B in the image transformer, text encoder not counted (the Hub metadata rounds to 6B). The bf16 transformer is 11.1 GB, which matches about 5.55B | [model card](https://huggingface.co/black-forest-labs/flux-3-image-klein), file sizes |
| Text encoder | Qwen3-VL-4B-Instruct (Apache-2.0), 8.9 GB in bf16. Hidden states of 8 of its 36 layers are stacked (8 x 2560) and projected to 3072 | [Diffusers blog](https://huggingface.co/blog/flux-3) |
| VAE | The FLUX.2 VAE, unchanged: 8x per side, 32 channels, 2x2 packed, so one token is 16 x 16 px. The ComfyUI repo ships it as `flux2-vae.safetensors` (336 MB) | [Diffusers blog](https://huggingface.co/blog/flux-3), Hub API |
| Transformer | Stage 1: 5 blocks each for the text, image and reference streams. Stage 2: 28 joint blocks. Text and reference tokens are modulated at timestep 0 | [Diffusers blog](https://huggingface.co/blog/flux-3) |
| Sampling | Distilled: 16 steps, no CFG, a flow-map model (each step also gets the timestep it jumps to). Time shift 6.93 | [Diffusers source](https://github.com/huggingface/diffusers/tree/main/src/diffusers/modular_pipelines/flux3) |
| Licence | FLUX Kommunity License v1.0. Free for non-commercial and non-production use. A "Qualifying User" (under US$5M gross annual revenue, affiliates included) may also use outputs commercially. NC is fine for this app | LICENSE.md in the repo |

The licence is a different text from the "FLUX non-commercial" licence of FLUX.2 Klein 9B. Points that
matter here:
- Section 1.c says use "in direct interactions with or that has impact on end users" is not a
  Non-Commercial Purpose. The app downloads the weights on the user's own machine or into the user's
  own Modal account, so the user is the one who runs the model, as with Klein 9B today. This is a
  reading, not legal advice.
- Section 2.e: a Qualifying User who uses outputs under 2.a must have content filters or review
  outputs. Not relevant to a non-commercial use.
- Section 3: anyone who redistributes the weights (a quantised copy, for example) must ship the
  licence and a notice. The community quantisations listed below do.

## 2. Inpaint support

**There is no inpaint pipeline and no mask input.**

- Diffusers support landed on `main` on 15 October 2026
  ([PR #15046](https://github.com/huggingface/diffusers/pull/15046)), after the last release
  (0.41.0, 6 October). So it needs Diffusers from git. Two more commits followed: the prompt
  upsampler block (#15058, 16 October) and "match the released BFL reference image sizing"
  (#15073, 19 October).
- It is a **Modular Diffusers pipeline only**: `ModularPipeline.from_pretrained(...)`, blocks
  `Flux3AutoBlocks` and `Flux3DistilledAutoBlocks`, transformer `Flux3Transformer2DModel`. There is
  no classic `Flux3...Pipeline` class, no `mask_image`, no `strength`. I read every file in
  `src/diffusers/modular_pipelines/flux3/` on `main`: no block reads a mask.
- The BFL GitHub, the BFL API docs ([docs.bfl.ml](https://docs.bfl.ml/flux_3/flux3_image_klein))
  and the model card list text to image, editing with up to 10 reference images, and bounding
  boxes. Boxes "set placement and scale, not a hard mask". No fill or erase endpoint.
- The model card says not to use the model without its prompt upsampler: short prompts give
  generic output. A pipeline built from `from_pretrained` runs the upsampler on every call unless
  the `prompt_upsampler` sub-block is popped.

How it differs from what the app uses now (`Flux2KleinInpaintPipeline`, `deploy/cloud/common/flux.py`):

| | FLUX.2 Klein (now) | FLUX 3 Image Klein |
|---|---|---|
| Pipeline | classic `Flux2KleinInpaintPipeline` (Diffusers 0.37+) | modular only, Diffusers git `main` |
| Mask | `mask_image`, latents outside the hole held each step | none; we would write the hold ourselves |
| Steps | 4 | 16 (4 times the transformer calls) |
| Reference image | concatenated into the image stream | its own 5 stream blocks, modulated at t = 0 |
| Prompt | the recipe prompt | wants a long scene caption; prompt padded to a multiple of 80 tokens, one prompt per call |
| VAE, token grid | FLUX.2 VAE, 16 px | same, so `LATENT_STRIDE`, `working_size()` and `HOLE_GROWTH` carry over |
| Text encoder | Qwen3-4B (4B) / Qwen3-8B (9B) | Qwen3-VL-4B |

What a masked render would need: a custom block in the denoise loop, after
`Flux3LoopAfterDenoiser`, that sets `latents = m * latents + (1 - m) * ((1 - s) * x0 + s * noise)`,
where `m` is the hole on the 16 px token grid, `x0` the packed latents of the (LaMa filled) crop and
`s` the next timestep. That is the same latent hold the sidecar's mflux backend already writes by
hand for Klein 4B (`_generate_masked` in `sidecar/manga_cleaner_sidecar/backend/mflux.py`). Modular
Diffusers is built for inserting such a block, but it is new code with no upstream test, and the
flow-map sampler has not been tried with a hold by anyone that I found.

## 3. Local

### Apple Silicon (mflux)

- mflux **0.23.0** (16 October 2026) runs FLUX 3 Image Klein distilled and Base: `mflux-generate-flux3`
  and `mflux-generate-flux3-edit`, 4, 6 and 8 bit quantisation, LoRAs
  ([release](https://github.com/mflux-community/mflux/releases/tag/v.0.23.0),
  [model README](https://github.com/mflux-community/mflux/blob/main/src/mflux/models/flux3/README.md)).
  No mask: the loop is a plain flow-map Euler step (`Flux3FlowMapScheduler.step`), so a hold would be
  hand-written, as for Klein 4B now.
- The app pins mflux **0.18.1** (`sidecar/pyproject.toml`). Going to 0.23.0 moves the Klein 4B
  path too: `_generate_masked` imports mflux internals (`_Flux2KleinEditHelpers`), and 0.23.0 now
  loads built-in models from a pinned commit (#872).
- mflux's own measurements, M4 Pro 64 GB, distilled, 1024 x 1024, 16 steps, text to image:

| Quant | On disk | Peak memory | Per step |
|---|---|---|---|
| bf16 | 20 GB download | 30.7 GB | 6.4 s |
| `-q 8` | 9.2 GB | 22.8 GB | 7.0 s |
| `-q 4` | 5.0 GB | 18.5 GB | 7.0 s |

  About 105 s per image including load. "Quantization saves memory, not time."
- MLX weights already on the Hub (ungated, licence copied): `vanch007/FLUX.3-Image-Klein-mlx-4bit`
  (6.1 GB), `-mlx-8bit`, `-mlx-bf16`; `VisualInference/FLUX-3-Image-Klein-MLX-6bit` and `-8bit`;
  `cguldogan/flux3-klein-mlx-4bit` and `-8bit` (the mflux port's author). Or `mflux-save` a q4
  copy ourselves.
- **Estimate for this app:** the working size is 768 on the long side (`WORK_LONG_SIDE`), so one
  768 x 768 crop is 2,304 image tokens plus 2,304 reference tokens, close to the 4,096 of a
  1024 x 1024 text to image. Expect about 7 s per step, so about 110 s per crop at 16 steps.
  Today's local Klein 4B takes 16 to 49 s per crop with a 7.5 to 9.1 GiB peak
  (`docs/research/manga-inpaint-models.md`). The q4 peak of 18.5 GB is above the sidecar's MLX
  cap (`set_memory_limit(12e9)`, declared working set 10 GiB), so a 16 GB Mac is out and a 32 GB
  Mac is tight.

### Windows and Linux (sdnq, sdcpp)

- SDNQ: `OzzyGT/Flux_3_Image_Klein_sdnq_dynamic_4bit`, revision `bf0af016a936bfe422eec96557aee058c3a6174e`,
  ungated, 6.65 GB in all (transformer 3.24 GB, Qwen3-VL 3.06 GB, VAE 0.34 GB bf16). Needs
  sdnq 0.2.8 or later and Diffusers with FLUX 3. "The sample below peaked at 11.5 GiB of VRAM at
  2048x1152" (GPU not named). There is an 8 bit twin. OzzyGT is a Diffusers team member; the
  recipes repo has a group offload script for the lowest VRAM
  ([diffusers-recipes](https://github.com/asomoza/diffusers-recipes/blob/main/models/flux3_image_klein/README.md)).
  No Disty0 build yet (Disty0 made both Klein SDNQ builds the app pins).
- The app pins `diffusers>=0.37.0`, `sdnq>=0.1.3` locally and `diffusers==0.39.0`, `sdnq==0.2.4`
  in the cloud image (`deploy/cloud/modal/app.py`). Both would need Diffusers from a git commit and
  sdnq 0.2.8.
- BFL's own int8 file targets 8 GB RTX 30 cards (ComfyUI only; BFL's code has no int8 path). FP8
  needs Ada or newer, NVFP4 needs Blackwell.
- stable-diffusion.cpp supports FLUX 3 Image Klein since
  [PR #2143](https://github.com/leejet/stable-diffusion.cpp/pull/2143) (merged 15 October).
  GGUFs: `unsloth/flux-3-image-klein-GGUF` (Q4_K_M 3.67 GB). Whether sd.cpp's mask option works
  with this model is not checked. The app's `backend/sdcpp.py` is a Klein 4B GGUF stub today.

## 4. Cloud (Modal)

- **Weights.** Pin the ungated SDNQ 4 bit repo above: 6.65 GB plus the 207 MB LaMa adapter, about
  6.86 GB. That is about half the 9B snapshot (12.8 GB) and one fifth more than the 4B (5.7 GB). The
  official bf16 set needs the transformer, text encoder and VAE folders (20.3 GB) and a Hugging
  Face token for the gate; skip `model.safetensors`, `ae.safetensors` and `variants/` (another
  22.9 GB in the same repo).
- **GPU memory.** bf16: 20.7 GiB peak on an H100 after an image to image call (Diffusers blog);
  fp8r 16.3 GiB; nvfp4 15.3 GiB. SDNQ 4 bit: 11.5 GiB at 2048 x 1152. At the 768 working size a
  4 bit render should sit well under 16 GiB (estimate), so one copy fits an L4 (24 GB), maybe two.
  bf16 wants an L40S.
- **Speed.** Distilled on an H100, bf16, 16 steps: 1.93 s text to image and 3.87 s with one
  1024 x 1024 reference (Diffusers blog); BFL quotes about 3 s on an H200 at 1 MP. **Estimate:**
  at 768 with a reference, about 2.2 s on an H100 and 5 to 6 s on an L40S, before SDNQ's
  dequantisation cost. Klein 9B v2 takes 2.2 to 3.1 s warm on an L40S, so FLUX 3 would cost about
  2 times as much per crop.
- **How it slots in.**
  - `deploy/cloud/common/flux.py`: a new runner beside `SdnqFluxRunner`. It reuses `LamaFill`,
    `working_size`, `HOLE_GROWTH`, `filter_call_kwargs`, `flux_prompt` and the output conversion.
    `load()` builds the modular pipeline, pops `prompt_upsampler`, adds the hold block to the
    denoise loop and moves to CUDA once. `render_png()` keeps the Cloud Klein v2 order: grow the
    hint by 8 px, fill with LaMa, resize to the working size, render 16 steps with the hold, resize
    back with BOX. Whether the filled crop also goes in as a reference (`image`) is a bench
    question: the reference has its own stream here, and v1 showed a reference with lettering
    draws the lettering back.
  - `deploy/cloud/common/manifest.py`: `MODEL_PROD_FLUX3`, `REVISION_PROD_FLUX3`, the pinned file
    list and total, recipe id `mc-flux3-klein-inpaint-v1`, pinned sampling (steps 16, guidance 1.0),
    licence "FLUX Kommunity", `runner="flux3"`, `adapters=(LAMA_ADAPTER,)`, and
    `render_peak_vram_mib` once measured.
  - `deploy/cloud/common/weights.py`: nothing new; `snapshot_name`, `check_snapshot_files` and the
    marker already key on the model id. One thing to check: this repo has
    `modular_model_index.json`, not `model_index.json`, so any "is this a pipeline folder" test
    (`sidecar/bootstrap.py`, `backend/sdnq.py` `_is_pipeline_dir`) needs the new name.
  - `deploy/cloud/common/worker.py`: dispatch the new `runner` value.
  - `deploy/cloud/modal/app.py`: a separate image for this model (Diffusers at a pinned git commit,
    sdnq 0.2.8, a `transformers` with Qwen3-VL), so the Klein and Qwen recipes keep their pins.
  - Client: recipe id in `crates/cleaner-core/src/cloud_wire.rs`, model name and picker in
    `src/lib/model/model-names.js` and `pipelines.js`, `RenderRate` in
    `src-tauri/src/inference/cloud_clean.rs`.
  - Tests: `deploy/cloud/tests/test_worker.py`, the weights tests, and `test_recipe_parity.py`
    (cloud only at first, as Qwen is).

## 5. Public manga or text removal results

None found. The model is four days old. The one manga text removal benchmark
([Furuta, arXiv 2603.27790](https://arxiv.org/abs/2603.27790)) predates it; it put Klein 9B at
26.9 PSNR. No scanlation tool (Koharu, MangaTranslator, BallonsTranslator) supports it yet.

One related community result: the author of `ausboss/FLUX-3-Image-Klein-Outpaint-LoRA` found
that without the LoRA the model "zooms in on the picture or slides it instead of extending it" in
36 of 40 runs, and mflux measured an 86 px shift (a 7% zoom) on a padded picture. So the edit mode
does not keep pixels in place by itself. A latent hold fixes the pixels outside the hole, but the
model may still draw the hole for a slightly shifted picture, which would show as seams.

## Integration plan

**Cloud first, as a bench (no app change).**
1. Scratch Modal app on the onlybixi profile, pinned SDNQ 4 bit repo, Diffusers at commit
   `4df4a5b3` or later, sdnq 0.2.8. Write the hold block.
2. Run the 14 Cloud Klein v2 regions (two black and white pages) and the 12 `08-058.png` regions.
   Variants: hold with the filled crop as reference; hold with no reference; 16 steps and 8 steps;
   the recipe prompt against a long caption. Compare with Klein 9B v2 by eye, `ink_left` and
   `out_diff`, and log seconds per crop and peak VRAM, on an L4 and an L40S.
3. Cost estimate: one L40S for about 20 minutes with cold loads, under US$1.
4. Only if it beats Klein 9B v2 on screentone and sound effects: build the cloud runner, manifest
   entry, image, client recipe and tests listed in section 4.

**Local after that, only if the cloud bench wins.**
1. Windows and Linux: `backend/sdnq.py` gains a FLUX 3 path with the same hold block and the
   SDNQ 4 bit repo; `requirements-sdnq.txt` pins Diffusers from git and sdnq 0.2.8;
   `sidecar/bootstrap.py` gets a model entry.
2. Apple Silicon: bump mflux to 0.23.0, re-test the Klein 4B masked path first, then add a FLUX 3
   hold loop in `backend/mflux.py` with a q4 MLX build. Measure on the 32 GB Mac before setting
   memory caps.
3. `crates/cleaner-core/src/engines/flux.rs`: `STEPS` (4) is Klein specific; steps become per
   model. Model ids in `crates/cleaner-core/src/sidecar/` and `engines/render.rs`.

## Open risks

- No mask anywhere upstream. The hold block is our own code on a flow-map sampler.
- Diffusers git `main` only; the cloud image pins 0.39.0 and the classes may still change
  (three commits in five days).
- 16 steps against 4: about 2 times the GPU cost of Klein 9B in the cloud (estimate), and about
  2 minutes per crop on a Mac (estimate).
- Local memory: 18.5 GB peak at q4 is above the sidecar's 12 GB MLX cap.
- The model expects long captions from a 9B upsampler. A short removal prompt may give generic
  fill. Running the upsampler would add 18.8 GB and a second model.
- The edit mode shifts and zooms pictures; seams at the hole edge are possible.
- No manga evidence at all. Colour drift on black and white pages, as with Qwen, is untested.
- Official repos are gated; the plan depends on community quantisations (OzzyGT, vanch007),
  which could change or vanish. Pin revisions.
- Licence: FLUX Kommunity, NC fine; section 1.c's "end users" wording deserves a look.

## Bench 1 (2026-10-19): two real pages, Modal L40S

The two black and white pages the user sent (984 x 1400, the same pair as Cloud Klein v2).
Detected headlessly by the app's own pipeline (CTD, Ogkalu RT-DETR Full, SAM-TS-L, all text,
outside clean, Hayai reader) with a mask padding of 11 px. The wide padding merged each sound
effect into one region: 4 regions, all outside balloons (3 on page 1, the large one on page 2).
The black wedge strokes at the top left of page 2 were not detected. Crops cut by
`load_cloud_region` and `CloudRegion::prepare` (preprocessing 2.0.0), the same input for every arm.

Arms, seed 1, one L40S each (profile onlybixi, throwaway app `mc-f3-bench`):
- `k9`: the production `SdnqFluxRunner` on Klein 9B, recipe v2 (LaMa fill, 4 steps).
- `f3_ref16`: FLUX 3 SDNQ 4 bit (OzzyGT, Diffusers `4df4a5b3`, sdnq 0.2.8), the same LaMa fill,
  the filled crop as the reference image, latents outside the hole held to it at every step
  (a patch on `Flux3LoopAfterDenoiser`), 16 steps, the recipe prompt.
- `f3_ref8`: the same at 8 steps. `f3_noref16`: the hold with no reference image.

| Arm | s/crop (warm) | Peak VRAM | Change from LaMa inside the mask (0 to 255) | By eye |
|---|---|---|---|---|
| `k9` | 2.9 to 4.6 | 13.6 GiB | 1.5 to 22.8 | clean, LaMa fill sharpened |
| `f3_ref16` | 5.5 to 9.3 | 8.0 GiB | 2.3 to 20.1 | the same as `k9` |
| `f3_ref8` | 3.0 to 5.3 | 8.0 GiB | 2.6 to 20.3 | the same as `f3_ref16` |
| `f3_noref16` | 3.2 to 5.1 | 8.0 GiB | 35.7 to 102.1 | **fails**: draws new figures and faces into the hole |

- All four sound effects came out clean on `k9`, `f3_ref16` and `f3_ref8`. At full size the
  three are hard to tell apart: both models keep LaMa's fill and sharpen the speed lines and
  screentone a little. FLUX 3 against Klein 9B inside the mask: a mean of 1.4 to 17.2 grey levels.
- No shift: the largest whole-crop shift outside the mask was 0.02 px. The edit mode's zoom
  problem does not show with the hold.
- Without a reference the hold alone gives FLUX 3 too little context: it draws a new picture.
- `ink_left` (0.28 for every arm) and `out_diff` (13.0 for every arm) do not separate the arms: the
  11 px mask takes in dark speed lines, and the out-of-mask change is the VAE round trip and resize.
- Load: 12.6 s for FLUX 3, 35.7 s for Klein 9B (no GPU snapshot in either).
- Spend: about $0.25 (Modal billing report, onlybixi, 2026-10-19), weights download included.

Reading: on these 4 regions FLUX 3 is as good as Klein 9B, not better. At 8 steps it costs about
the same time. It needs 5.6 GiB less VRAM, so it would fit an L4 with room for a second copy.
4 regions on 2 pages is a tiny sample; colour pages and balloons were not in it.

Not measured: balloon text, colour pages, other seeds, an L4, guidance prompts, 4 steps.

## Bench 2 (2026-10-19): two colour webtoon pages, targeted prompts

Two colour Korean webtoon pages the user sent (690 x 1600), detected the same way with 11 px padding:
5 regions, 2 sound effects over art (page 1) and 3 narration boxes on painted backgrounds (page 2).
Every text area was found. Each region got a hand-written guidance target and description (the
app's `QwenEdit`, turned into a prompt by `flux_prompt`), for example "a dark blue brick wall with
the same bricks and mortar lines as the rest of the wall".

"A smarter text encoder" cannot mean a bigger encoder: the FLUX 3 transformer reads 8 x 2560
hidden states of Qwen3-VL-4B, so another model does not fit. The repo's encoder files are
byte-identical to `Qwen/Qwen3-VL-4B-Instruct` (same LFS hashes), so this bench used that repo at
full bf16 instead of the 4 bit copy, and added BFL's 9B prompt upsampler
(`black-forest-labs/flux-3-image-klein-pup`, Apache-2.0, ungated). It sees the crop with its lettering
and expands the guided prompt into a long caption.

Arms, seed 1, LaMa fill first in every arm, one L40S each:
- `k9_auto`, `k9_guided`: production Klein 9B v2, without and with the region's guidance.
- `f3_auto`, `f3_guided`: FLUX 3 (4 bit transformer, bf16 encoder), recipe prompt and guided prompt, 16 steps.
- `f3_guided_te4`: the guided prompt through the 4 bit encoder.
- `f3_pup`: the upsampler's caption through the bf16 encoder.

By eye:
- Narration boxes on painted backgrounds (clouds, grainy gradient, brick wall): every arm is
  clean and they look almost the same. All of them stay close to the LaMa fill; neither model
  rebuilds the brick pattern or the grain much better than LaMa.
- `콰작` over a broken stone column: the auto arms of both models are LaMa smudge. Klein 9B
  with guidance drew white crack lines; FLUX 3 with guidance did not; FLUX 3 with the upsampler
  caption drew crack lines too.
- `쩌저적` over a creature and light bursts: **Klein 9B with guidance drew a large new grey
  creature body into the hole.** FLUX 3 with guidance drew only the asked-for lightning. The 4 bit
  encoder arm drew the lightning plus a stray grey blob.
- So guidance helps only where the hole is busy art, and a description that names an object
  makes Klein 9B draw that object. FLUX 3 followed the description with more restraint here.
- The bf16 encoder against the 4 bit one: a small difference, one stray blob. Not decisive.
- The upsampler: 10 to 14 s per crop on top, 18.8 GB more weights, and its captions were wrong
  twice (it called a colour panel black and white, and misread the Korean sound effects). Not worth it.

Numbers: warm seconds per crop, Klein 9B 3.0, FLUX 3 at 16 steps 4.8 to 6.5 (11.4 for the first
crop). Peak VRAM, Klein 9B 13.1 GiB, FLUX 3 17.9 GiB with both encoders held. Spend for benches
1 and 2 together: about $0.53 (onlybixi).

Not measured: other seeds, black-and-white balloons, FLUX 3 at 8 steps on colour, an L4.

## Go or no-go

No-go for now: on the first bench FLUX 3 matched Klein 9B but did not beat it. Worth one more bench (balloons, colour, an L4, 4 to 8 steps) only if the lower VRAM matters.
