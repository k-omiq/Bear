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

## Go or no-go (2026-10-19)

No-go for now: on the first bench FLUX 3 matched Klein 9B but did not beat it. Worth one more bench (balloons, colour, an L4, 4 to 8 steps) only if the lower VRAM matters.

## What benches 1 and 2 got wrong

Both benches measured LaMa, not FLUX 3:
- **The reference was the LaMa-filled crop.** FLUX 3 is an edit model. With the lettering already
  gone from its reference and every token outside the hole held to that reference, it could only
  sharpen LaMa's guess. That is why every arm looked "the same as `k9`".
- **The time shift was 6.93.** BFL changed its reference sampler to 16.0 on 2026-10-17. Diffusers
  0.42.0 still builds the scheduler with 6.93, so the benches sampled a schedule BFL no longer uses.

## Bench 3 (2026-10-26): the edit recipe, 17 real regions, Modal L40S

The recipe now in `deploy/cloud/common/flux3.py`: the real crop, lettering included, as the
reference; a prompt that says where the lettering is; BFL's sampling (16 steps, shift 16.0, noise
drawn as CPU tokens and cast to bf16, the autoencoder in float32); no LaMa, no prompt upsampler.
The prompt is the one the user brought from another manga tool on 2026-10-26, with `{location}`
filled from the hint ("upper right").

Input: 17 regions the app's Detect run would send to the cloud from pages 182, 185 and 187 of the
real chapter (`spike-cloud-crops export`, preprocessing 2.0.0): balloons and narration boxes,
and one sound effect over speed lines. The gate held the other sound effects back. (An earlier
count here, 15 balloons and boxes, left one of the 17 unnamed; the crop list is gone, so only the
17 stands.)
Profile aizendazai46, throwaway app `mc-f3edit-bench`, one L40S. The live Qwen v4 recipe on the
same crops is the baseline.

| Arm | Change | s/crop (median) | Peak VRAM (torch) | Black or grey fill in a white balloon |
|---|---|---|---|---|
| `words_s1` | user's prompt as given, seed 1 | 5.6 | 8.4 GiB | 2 of 17 (182_00, 182_03 painted black) |
| `plain_s1`, `plain_s2` | without ", including black or shaded fills", seeds 1 and 2 | 5.6 | 8.4 GiB | 0 of 34 |
| `plain_hold` | as `plain_s1`, tokens outside the grown hint held | 5.8 | 8.4 GiB | 0 of 17 |
| `plain_box` | the place as a FLUX 3 target box instead of words | 5.7 | 8.4 GiB | 3 of 17 |
| `plain8` | as `plain_s1`, 8 steps | 3.0 | 8.4 GiB | 0 of 17 |
| `qwen4` | live Qwen v4 recipe | 16.2 | 21.4 GiB | not comparable: see below |

An earlier probe on 3 crops and 4 seeds gave the same picture: the user's sentence painted one
balloon black in 12 renders, no fill sentence left grey blotches in 4 of 12, and the shortened
sentence none in 12. So the recipe drops that clause: 0 of 80 renders across seeds 1 to 4.

By eye:
- **FLUX 3 keeps every balloon and its outline and removes only the lettering.** Furigana,
  small kana and the one sound effect came out clean. Lettering outside the hint (a `キッ`
  beside a balloon) was left alone, as the prompt asks.
- **Qwen v4 erased whole balloons** on 8 of the 17 crops and drew art where they were (a figure,
  a tree, clouds, a wall). The app writes only inside the lettering's mask, so on the page that
  shows as patches of new art inside a white balloon.
- **The hold hurts.** On 185_00 the hint did not cover all the lettering, and the hold kept what
  it missed. Off by default.
- **The target box hurts.** It painted three balloons grey or black.
- **8 steps look the same as 16** at full size on these crops. The recipe keeps BFL's 16: the cost
  is about half a cent a crop.

Speed: 11.4 s per working megapixel at 16 steps (11.8 at most), 6.2 at 8; Qwen 40. Load 14 to
26 s. Spend: about $0.70 for the seed download, a smoke run, two probes and this bench (estimated from GPU time).

Not measured: host memory and nvidia-smi VRAM (so one copy per container, 24 GiB, the bench's
size), a black or shaded balloon, colour pages, Beam, an L4.

## Bench 3b (2026-10-26): post-processing against truth

The other tool's post-processing (grayscale, global then local tone, Poisson lighting, the page's
own screentone, contour protection, the write mask), prototyped in Python and then ported to
`crates/cleaner-core/src/engines/postprocess.rs` as preprocessing 3.0.0. Real lettering has no
truth under it, so outlined katakana sound effects were drawn over real pages (black fill, white
outline, rotated) and FLUX 3 removed them (the recipe above, seed 1). Scores inside the write mask
against the page as it was: mean absolute error, the same after a 2.5 px blur (tone), and the
correlation of the 1.5 px detail (1 = dots in phase).

| Set | Raw composite | Rust 3.0.0 |
|---|---|---|
| 15 crops, five halftone pages (`spikes/screentone-fill/corpus`) | 31.0 / 11.2 / 0.48 | 27.2 / 7.7 / 0.57 |
| 16 crops, pages 180 to 188 (hand-drawn texture) | 34.7 / 17.4 / 0.44 | 34.3 / 16.2 / 0.44 |

- The screen step ran on 5 of the 15 halftone crops and none of the others: the hand-drawn
  texture never repeats at 0.8. Where FLUX 3 painted a flat grey blob over a dot screen
  (wizard2-21), error went from 42.6 to 18.3 and the dots came back in phase.
- Two crops got worse, by under one tone level; none by more.
- FLUX 3 itself was the larger error on the hard crops: it erased black strokes under the lettering
  and a strand of hair, which no post-processing restores.

## Bench 4 (2026-10-26): the official bf16 weights against the 4 bit copy

The other tool needs about 19 GB at its lowest setting. That matches the official bf16 set, not
the 4 bit copy the recipe pins. Same recipe, prompt, seed and crops as bench 3b; only the snapshot
differs: `black-forest-labs/flux-3-image-klein` at `337337621b8526e120f2275d250242f4a6bedd75`,
the transformer, text encoder and VAE folders (20.3 GB; gated, licence accepted on the user's
Hugging Face account). One L40S per arm, in parallel, profile aizendazai46. The 4 bit arm gave
bench 3b's answers again bit for bit. Scores as in bench 3b (mae / tone / screen).

| Set | 4 bit raw | bf16 raw | 4 bit, Rust 3.0.0 | bf16, Rust 3.0.0 |
|---|---|---|---|---|
| 15 halftone crops | 31.0 / 11.2 / 0.48 | 28.4 / 9.3 / 0.55 | 27.2 / 7.7 / 0.57 | 25.7 / 6.7 / 0.60 |
| 16 chapter crops (180 to 188) | 34.7 / 17.4 / 0.44 | 34.2 / 16.8 / 0.47 | 34.3 / 16.2 / 0.44 | 33.9 / 16.0 / 0.47 |

- Halftone: bf16 had the lower error on 13 of 15 crops raw and the higher on none (0.3 level
  margin); 10 and 1 after the 3.0.0 post-processing. It keeps more of the dot screen under the
  lettering (wizard2-21_s0 screen 0.01 to 0.38 raw). Chapter pages: 8 better, 3 worse.
- 17 real balloon crops (pages 182, 185, 187): both clean. Faint ghosts of the letters stay in the
  white balloons, 1 to 2 levels darker on average inside the lettering, except 182_04 (4 bit 11.3,
  bf16 3.8). Neither redraws what FLUX 3 drops: sample_s1's third circle is missing in both.
- At these sizes (working long side 768): bf16 5.6 s mean, 7.3 s max, 21.1 GiB peak, 19.0 GiB
  after load; 4 bit 6.6 s mean, 15.3 s max, 8.4 GiB peak. bf16 is not slower on an L40S. Its
  21.1 GiB peak is close to an L4's 24 GB, which was not tried.
- Spend: $0.53 (Modal billing report), test volume deleted.

bf16 is better, most on screentone, but by a few levels, not by a wide margin. It does not on its
own explain a much better result in the other tool.

Tone. FLUX 3 (bf16) moves the brightness of the page it redraws: away from the mask, the 4 px
low-pass of its answer is off the page's by under 4 levels at the median but 10 to 20 levels
brighter in the brightest tenth on most crops. The resize to and from the working size accounts
for under 1 level of that (3 on blackjack1-016). The 3.0.0 post-processing takes the error inside
the mask from 9.3 to 6.7 levels on halftone; it is worst on sample_s0 and sample_s1, where the gain
and offset both hit their bounds (1.15, -32) and sample_s1 ends 10 levels too dark. A prototype that
pulled each background region's tone inside the mask to a membrane of the page's own tone in that
region, after 3.0.0, was worse on 14 of 31 crops and better on 5, and was dropped.

## Bench 5 (2026-10-26): the screen step redrawn, working size and prompt arms

The other tool's results on real pages keep the screentone under removed lettering; ours left
flat grey patches. The 3.0.0 screen step (repeat offsets at a correlation of 0.8, then a copy)
ran on 5 of 15 halftone crops. The test pages' screens are fine: dots 2 to 4 px apart, most at 45
degrees, so their frequency is past half the sample rate along the diagonal, and integer repeat
offsets miss them by a third of a cycle.

The screen step is now `crates/cleaner-core/src/engines/halftone.rs` (prototyped in Python first).
Per background region next to the mask: the strongest spectral peaks of the page's fine detail,
each refined by the turn of its demodulated phase; the page's local phase at each, carried into
the mask from coarse to fine; for each pair, a table of the page's value by place in the dot cell
and tone, built on alternate 16 px blocks and scored on the others; the best pair redraws the
mask at the answer's tone if it scores 0.5. Same crops, seed and scores as bench 3b (mae / tone /
screen), bf16 answers at the 768 working size:

| Set | 3.0.0 as it was | screen step redrawn |
|---|---|---|
| 15 halftone crops | 25.7 / 6.7 / 0.60 | 19.2 / 6.9 / 0.73 (14 better, 0 worse) |
| 16 chapter crops (180 to 188) | 33.9 / 16.0 / 0.47 | 31.9 / 16.0 / 0.54 (7 better, 0 worse) |

The 4 bit answers gain the same (halftone 27.2 to 19.5, 14 better; chapter 34.3 to 32.2, 7
better; none worse). The chapter pages have screens after all: the old step never found them. A
held-out score of 0.8 left 5 halftone crops untouched that 0.5 improves; 0.4 changes nothing more.
Run time in a release build: 0.5 s mean, 0.9 s worst per 448 px crop (whole post-processing).

FLUX 3 arms through the new post-processing, bf16, one seed, each against the 768 working size
(mae, crops better / worse at a 0.3 level margin, median change over all 31):

| Arm | Halftone | Chapter | Median |
|---|---|---|---|
| working long side 1280 | 17.1, 11 / 4 | 30.9, 10 / 3 | -1.8 |
| 1024 | 19.1, 6 / 9 | 26.7, 6 / 9 | +0.7 |
| 1024, screentone sentence | 19.3, 6 / 8 | 32.8, 5 / 11 | +1.1 |
| 768, screentone sentence | 19.4, 5 / 5 | 34.0, 1 / 6 | +0.1 |
| 768, rewritten prompt | 19.1, 6 / 4 | 27.9, 3 / 6 | +0.1 |
| 4 bit, 768 | 19.5, 2 / 6 | 32.2, 4 / 6 | +0.3 |

- The sentence asking for the screen to continue ("never as flat grey") made it worse. The
  rewritten prompt moved two outliers and little else. The user's prompt stays.
- 1280 is the only arm with more crops better than worse on both sets (11 / 4 and 10 / 3) and a
  lower median. By mean alone, 1024 and the rewritten prompt are also below 768 on both sets, but
  1024 is worse on more crops than it is better, so it is not a smooth trend; one seed.
  At 1280 a 448 px crop takes 20.1 s on an L40S (768: 5.6 s) and peaks at 24.9 GiB in bf16.
- Neither the model arms nor the post-processing draw what the lettering hid completely
  (sample_s1's third circle).
- Spend: $1.32 (Modal billing report; estimated $1 beforehand), test volume deleted.

## Choices left to the user (2026-10-26)

Benches 4 and 5 found two better arms, each by a margin that depends on the page: the bf16
weights and a 1280 working long side. Neither is pinned in place of the old answer; both are
choices.

- Weights: setup lists FLUX 3 Image Klein (bf16) beside the 4 bit model, recipe
  `mc-flux3-klein-bf16-edit-v1`, otherwise the same recipe. BFL's repository is gated, so the seed
  reads `unsloth/flux-3-image-klein` at `67662613c10c92a1364cb3da8c6f7447977c7ce9` instead: its
  transformer, text encoder and VAE folders and licence, 23 files, 20.3 GB, each with the same
  SHA-256 as BFL's repository at `337337621b8526e120f2275d250242f4a6bedd75` (read from both Hub trees,
  the gated one with the user's token). The rest of that repository (a single-file checkpoint, fp8
  and nvfp4 variants, images; 23 GB) is not downloaded. L40S on Modal, RTX5090 on Beam: 21.1 GiB
  peak at 768 is too close to an L4's 24 GB.
- Working size: Auto clean offers 768, 1024 or 1280 for a FLUX 3 profile. The request carries it
  as `work_long_side` in the recipe, like the guidance text, and it is in the request digest; the
  worker refuses any other value and any other recipe. 768 is sent as nothing, so its requests
  are the ones a worker without the option takes; no FLUX 3 worker was deployed before it.

Live check (2026-10-26, profile aizendazai46): a throwaway installation of the bf16 model set up
by the real provisioner (plan, apply, cleanup), L40S, 60 s idle window, then 7 real crops sent
through its gateway one at a time. Spend $0.14 (Modal billing report); every resource deleted by
the provisioner's own cleanup.

- Setup, the 20 GB seed from unsloth's copy included, took about 4 minutes and passed every step
  (file sizes, health, model info naming `mc-flux3-klein-bf16-edit-v1`).
- At the default size the 4 answers (182_00, 182_04, 185_03, 187_00) are bit for bit bench 4's bf16
  answers from BFL's own repository: same weights, same recipe, same worker.
- Wall time per crop through the gateway, submit to completed: 47 s for the first (cold start and
  load), about 10 s warm at 768 (one 768 crop took 22 s), 16 s at 1024, 23 s at 1280.
- A requested size reached the worker: 1024 and 1280 gave other answers. On 182_04 the 768 answer
  also erased a hand-drawn sound effect outside the hint, and the 1280 answer left it; the mean
  change outside the hint was 14.7 levels at 768 and 5.7 at 1280 (187_00: 8.7 and 4.7). The client
  writes only inside the mask, so this is how faithful the answer is, not what reaches the page.
- Found and fixed: the plan's note said a cold start loads 5.5 GB of weights for every model; it
  now states the chosen model's size (20.3 GB here).

## Bench 6 (2026-10-26): 3.0.0 on local LaMa and cloud Qwen v4

Whether the 3.0.0 post-processing (`postprocess::settle`) helps the two other cleaning models.
Same 31 synthetic crops, scores and 0.3 level margin as benches 3b and 5 (mae / tone / screen
inside alpha > 0). Every arm is written through the same `alpha.png` in 8-bit space, and every
answer is read as the app reads a grey page's answer (the mean of R, G and B). A temporary ignored
test in `engines/lama.rs` ran the app's own code; the file was restored from its backup and
matches it byte for byte. Re-run this way, FLUX 3 bf16 gives bench 5's numbers to 0.1 level, so
its rows below are the reference.

**LaMa** ran through the rung's own steps: the region's `Fitted::ink` (here the hint, fitted as a
manual mask so that the app's write weight is `alpha.png`; the app's ramp matched it on every
pixel of all 31 crops), one 512 tile cut from the whole lettered page, the hole zeroed, and the
provider `Automatic` picks on this Mac (WebGPU, 0.6 to 0.9 s a crop). The rung does not write its
raw answer today: each tile goes through 2.0.0's tone curve first (`ToneSite` in
`Inpainter::render`). That is the "today" row; the app's own `Inpainter::render` matched it to 0.5
level on every crop. L2 is `screentone::render` on the same hole, written through its own mask
with no ramp, as the app writes it.

**Qwen** ran on a throwaway installation made by the real provisioner (profile aizendazai46, one
L40S, 60 s idle window): `Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32`, recipe
`mc-qwen-image-edit-2511-v4`, 8 steps, seed 1, `qwen_edit` target auto, each crop with its
`hint.png`. Q2 is 2.0.0's tone curve (`ToneSite::measure` and `ToneReport::corrected`) measured on
the 448 crop the model was shown. The app measures it on the V2 crop, 128 px wider a side, which
Qwen was not shown here; the ring it reads (6 to 18 px outside the applied mask) lies inside the
448 crop on every one of these.

| Arm | Halftone (15) | better / worse | median | Chapter (16) | better / worse | median |
|---|---|---|---|---|---|---|
| L0 LaMa raw | 16.0 / 6.8 / 0.84 | | | 24.5 / 10.6 / 0.65 | | |
| LaMa today (2.0.0 curve) | 15.7 / 6.2 / 0.84 | 5 / 0 | -0.24 | 24.3 / 10.3 / 0.65 | 4 / 0 | -0.14 |
| L1 LaMa, 3.0.0 | 14.5 / 6.1 / 0.85 | 10 / 2 | -1.52 | 24.1 / 10.5 / 0.65 | 7 / 2 | -0.23 |
| L1, 3.0.0 on the hole only | 14.7 / 7.1 / 0.85 | 11 / 2 | -1.47 | 23.9 / 10.5 / 0.66 | 7 / 2 | -0.10 |
| L2 Screentone engine | 27.3 / 15.5 / 0.65 | 0 / 14 | +10.21 | 36.1 / 23.8 / 0.47 | 2 / 14 | +7.34 |
| Q0 Qwen raw | 31.1 / 5.6 / 0.52 | | | 30.4 / 15.1 / 0.57 | | |
| Q2 Qwen, 2.0.0 (today) | 31.2 / 5.5 / 0.52 | 1 / 2 | +0.07 | 30.4 / 15.1 / 0.57 | 0 / 0 | 0.00 |
| Q1 Qwen, 3.0.0 | 17.5 / 5.0 / 0.76 | 14 / 0 | -14.46 | 28.4 / 15.1 / 0.64 | 8 / 0 | -0.35 |
| FLUX 3 bf16 raw | 28.4 / 9.4 / 0.55 | | | 34.1 / 16.8 / 0.47 | | |
| FLUX 3 bf16, 3.0.0 with bench 5's redraw (reference) | 19.3 / 6.9 / 0.73 | 14 / 0 | -6.45 | 32.0 / 16.0 / 0.54 | 9 / 0 | -0.49 |

Each model's rows count against its own raw arm. Against today's LaMa path, L1 is 10 better and
2 worse on halftone (median -1.12) and 5 and 2 on the chapter pages (median -0.06).

Mean error rewards blur, so detail strength was read too: the std of the 1.5 px detail inside the
mask over the truth's (1 = as crisp as the page). Halftone: LaMa 1.00 raw, 0.92 after 3.0.0; Qwen
1.04 and 0.89; FLUX 3 0.97 and 0.97. Chapter: LaMa 0.97 and 0.94; Qwen 0.86 and 0.88. At 4x
nearest-neighbour zooms Qwen's raw screen is grey mush and the settled one is crisp dots in phase,
so its gain is real. LaMa's dots are crisp and in phase before and after, a little lower in
contrast after, and a line through the hole a little softer: part of LaMa's halftone gain may be
the softening that mean error rewards.

LaMa:
- As expected, tone and lighting do little: the fitted gain stays within 0.96 to 1.02; the offset
  is up to 10 levels (LaMa returns some crops darker), which 2.0.0's curve also corrects. The
  screen step is the gain. It ran on 14 of 15 halftone crops and 9 of 16 chapter crops.
- The settle costs where LaMa was already right. LaMa keeps the page outside its hole, so in the
  5 px ramp band its answer is the page to within a few levels (mae 6.0 halftone, 5.0 chapter), and
  the screen step redraws that band too (6.8 and 5.7). Settling only the hole keeps the band (5.9,
  4.8) but loses the tone gain inside the hole (7.4 to 8.6 halftone); the totals come out the
  same.
- Worse at 0.3: blackjack1-016_s1 (15.9 to 16.5), wizard2-21_s1 (16.6 to 17.0), 181_s0 (70.4 to
  70.8) and 185_s1 (27.2 to 29.2, the screen redrawn over a grey LaMa painted where the page is
  black and white).

Qwen:
- Qwen keeps the page's tone (tone 5.6 on halftone, the best of the three raw) and loses its dots
  (screen 0.52); 3.0.0 puts them back in phase. On doratama-18_s0 Qwen drew a coarse dot screen of
  its own and the settle replaced it (35.8 to 7.6). Q2 changes almost nothing: Qwen's tone is not
  what is wrong.
- Drift guard. Two crops changed the drawing near the lettering: 185_s1 (46% of the page within
  64 px of the mask moved by more than 24 levels after the 2.5 px blur; Qwen painted black strokes
  white) and 184_s0 (29%; it carried a balloon's white into the grey). On 185_s1 the fit stayed at
  gain 1.0, offset +0.2, and the crop did not get worse. On 184_s0 the fit ran to both bounds (gain
  1.15, offset -23.9) and the crop still scored better (43.3 to 38.2), though the settle darkened a
  blotch Qwen had invented. No Qwen crop got worse.
- Bench 3's 17 real balloon crops with their Qwen v4 answers (no truth, so by eye and by how far
  the settle moved the patch): where Qwen drew art inside a white balloon (182_00, 185_01, 185_04,
  185_05, 187_00, 187_03, 187_04; bench 3 counted 8 such crops, and the eighth is not named in
  what is left of either bench), the settled patch is still that art. The guard kept the tone fit
  near no change on five of them (gain 1.00 to 1.03, offset -6.5 to 0), and on four of those the
  settle moved the patch by under 3 levels: 3.0.0 neither hides nor worsens the failure. Three moved by 16 to 17 levels inside the mask: 185_05 (offset +20.8, the
  drawn shape lightened toward the balloon's white), 187_04 (gain 1.08, offset -10, the shape
  darkened) and 182_00 (the screen step put dots into the drawn art). The faint letter ghosts
  Qwen leaves in clean balloons stay.

Side by side:
- On these crops LaMa raw is better than either cloud model after 3.0.0 (halftone 16.0 against
  17.5 and 19.3; chapter 24.5 against 28.4 and 32.0), inside the hole as well (halftone 19.5
  against Qwen 20.1 and FLUX 3 21.7). LaMa reads 512 px of the real page around the lettering and
  redraws only the hole. It drew sample_s1's third circle, which neither cloud model does.
- The Screentone engine was worse than LaMa on 28 of 31 crops and better on 2 (180_s0, 181_s0).
  On these large holes (64 to 96 px glyphs
  with 4 to 7 px outlines) its copy took sources from black panel borders (sample_s1,
  blackjack1-016_s2), and on the chapter pages it filled white paper with dots (183_s0).

Settle time, release build, per 448 px crop on this Mac: LaMa answers 0.47 s mean, 0.73 s worst;
Qwen 0.46 and 0.72; FLUX 3 0.48 and 0.79. For LaMa that is close to the model run itself.

Qwen run: 22.7 s median per crop through the gateway, submit to result (24.0 s at most), and 91 s
for the first with the cold start. No job ran long enough for a second seed, so each crop took one
render. Setup took about 2 minutes. Spend $0.63 (Modal billing report for the app; estimated $0.50,
$1.25 at most beforehand). The provisioner's own cleanup deleted the app, dict, volume and token,
and the endpoint key file was deleted.

What a change would be (neither made as a default; both are a setting since 2026-10-27, see "Finishing for Qwen, FLUX.2 Klein and LaMa" after
bench 9; the setting took neither path below):
- Qwen: `RECIPE_PROD_QWEN`'s `RenderRecipe` in `deploy/cloud/common/manifest.py` takes
  `PROD_EDIT_PREPROCESSING_VERSION`; `deploy/cloud/tests/test_recipe_parity.py` names the recipes
  on 3.0.0; the deployed Qwen profiles are redeployed so their gateways advertise it. The request
  does not change (V3 crops as V2), only the client's composite. Results prepared under 2.0.0 keep
  2.0.0. Whether the recipe id moves to a v5 depends on whether a deployed v4 gateway must keep
  advertising 2.0.0 to older clients.
- LaMa: in `Inpainter::render` (`crates/cleaner-core/src/engines/lama.rs`), per tile, in place of
  the `ToneSite` measure and `corrected` call: `postprocess::settle` on the tile crop as the model
  saw it, its answer, the pixels this tile writes (applied mask, alpha above 0) as the mask, and
  the in-page opaque pixels as the page; commit the settled values through the same ramp, and set
  `Rendered::matched`. `run::Made` drops the tone report today, so the provenance record needs
  carrying too. A per-tile settle on tiled regions (above 512 px) was not measured.

Sheets (crop, truth, raw, settled, Screentone, FLUX 3) are in the session scratchpad only; the
crops are copyrighted pages.

## Bench 7 (2026-10-26): the screen step copies the page's own dots

The user saw bench 5's redrawn screens as blurry: the pattern right, the dots soft. Bench 5's redraw
looked each mask pixel up in a table of the page's value by place in the dot cell and tone, an
average of many dots, and mean error rewards that average. The other tool's step 4 copies original
pixels through a phase-preserving shift and adjusts their brightness. The screen step now does that
(`engines/halftone.rs`):

- Shifts: every whole-pixel shift up to 64 px whose phase error on both screen frequencies is at
  most 0.05 cycle; the 16 closest in phase in each of four bands of length are tried, and kept
  where the page's detail one shift away correlates with itself by 0.8.
- Sources: page pixels of the region whose 5 x 5 patch matches the patch one shift away (mean
  difference at most 30 levels) for at least half the shifts. Without this check the copy took
  pieces of the page's own lettering, grid lines and hair edges.
- Copy: each mask pixel takes the source one shift away closest to the answer's tone, within 16
  levels, plus that tone difference. A pixel no shift reaches from a source keeps bench 5's table
  redraw.
- A second pass that hopped from copied pixels into the middle of wide masks drew faint streaks and
  dots in black areas by eye, so it was dropped.

A bug found on the way: the answer's contours inside the mask included FLUX 3's own moire stripes
(24% of the mask on wizard2-21_s0, which has no drawn line there), and every step fades out near a
contour, so the old redraw was held off most of that patch. The answer's contours are now found
under a 1.5 px blur (`DESCREEN_SIGMA` in `engines/postprocess.rs`).

Same 31 synthetic crops, bf16 answers at 768 (mae / tone / screen / sharp, where sharp is the std of
the 1.5 px detail inside the mask core over the truth's, 1 = as crisp as the page):

| Arm | Halftone (15) | Chapter (16) |
|---|---|---|
| Bench 5 redraw | 19.2 / 6.9 / 0.73 / 0.96 | 31.9 / 16.0 / 0.54 / 0.90 |
| Bench 5 redraw, contour fix | 17.7 / 6.9 / 0.76 / 0.96 | 31.5 / 16.0 / 0.56 / 0.91 |
| Copy, contour fix (shipped) | 18.3 / 6.8 / 0.76 / 1.01 | 31.6 / 16.0 / 0.56 / 0.92 |

- Against the old redraw: 7 halftone crops better, 5 worse; chapter 5 and 1. The 4 bit answers:
  19.5 to 19.0 (6 and 5), 32.2 to 31.8.
- Against the redraw with the same contour fix, the copy has 0.6 level more mean error and the same
  phase (0.76): crisp dots half a pixel off cost more than an average. On the blurriest crops sharp
  rises from the fixed redraw to the copy: blackjack1-016_s2 0.82 to 0.94, wizard2-21_s0 0.84 to
  0.98. By eye at 4x the copied dots match the page's contrast and size; the redraw's are fainter.
- Worst crop by error: wizard2-21_s2, 10.8 to 16.9 against the redraw with the fix, by eye clean
  dots with a small band where the copy changes source.
- Run time of the whole post-processing in a release build: 0.66 s mean, 1.19 s worst per 448 px
  crop (was 0.5 and 0.9).
- Bench 6's LaMa and Qwen rows used bench 5's redraw and were not re-run.

Not measured: real lettering, colour pages, 2048 px crops, a second seed, LaMa and Qwen through the
copy.

## Bench 8 (2026-10-26): every finishing, and the FLUX 3 generation options, as settings

Every post-processing version measured so far is a choice again (`Finishing` in
`crates/cleaner-core/src/engines/postprocess.rs`, Settings > Post-processing; the default is
bench 7's). Manga Cleaner Reproduction's finishing v2 is ported to Rust (`engines/finishing.rs`) and is a
choice too. Four FLUX 3 generation options are switches sent to the worker (`flux3_generation`,
docs/cloud-contract.md section 9; Settings > Cloud). Local only, no cloud spend.

**Port check.** The port was run on Manga Cleaner Reproduction's own two examples (its original crop,
raw answer at source size and mask) against its shipped outputs, inside the mask:

| example | off, global, local | poisson | raster |
|---|---|---|---|
| screentone | equal | mean 0.14, max 1 | mean 0.96, max 52 |
| redrawing | equal | mean 0.02, max 1 | equal (declines, same reason) |

Poisson's 1 level is OpenCV's float32 solve and byte truncation. Raster finds the same
frequencies (within 1.1e-4), pixel count (27596) and regions; the rest is numpy's random
sampler, which the port replaces with a seeded one of the same counts. With the port's sampler
patched into the Python, the two differ at 6 pixels by 1 level. One departure was added on
2026-10-27, after this check: the ported Poisson step takes the page mask, and where the crop is
off the page (transparent pixels, the unwritten part of a LaMa hole) the answer, not the crop, is
the clone's boundary; `finishing.py` has no page mask. Both examples are all page, so the table
above is unchanged by it. Besides the random sampler and float precision, that is the one place
the port departs from `finishing.py`.

**Fake lettering, bf16 answers** (mean of 15 halftone crops; mae, tone, screen correlation,
sharpness; old table redraw first for scale):

| finishing | halftone | chapter pages (mae) |
|---|---|---|
| redraw-sharp-contours (bench 5) | 19.2 / 6.9 / 0.73 / 0.96 | 31.9 |
| copy (bench 7, default) | 18.3 / 6.8 / 0.76 / 1.01 | 31.6 |
| copy-hops | 19.4 / 7.3 / 0.74 / 1.03 | 31.7 |
| redraw | 17.7 / 6.9 / 0.76 / 0.96 | 31.5 |
| repeat-offsets (bench 3b) | 25.7 / 6.7 / 0.60 / 0.97 | 33.9 |
| tone-only | 27.5 / 6.7 / 0.56 / 0.96 | 33.9 |
| as-is | 28.5 / 10.0 / 0.55 / 0.96 | 34.1 |
| global-tone | 27.9 / 7.9 / 0.55 / 1.00 | 34.1 |
| local-tone | 28.6 / 8.6 / 0.55 / 0.98 | 33.2 |
| poisson | 28.6 / 8.7 / 0.55 / 0.98 | 33.1 |
| dots-only | 26.7 / 12.1 / 0.65 / 1.05 | 33.8 |

The versions brought back reproduce their own benches exactly (redraw-sharp-contours 19.2 as in
bench 5, repeat-offsets 25.7 as in bench 4), and copy gives bench 7's 18.3, so nothing was lost
in bringing them back. The ported finishing does little on these answers: our FLUX 3 answers
lose the page's dots, and only its dots-only step puts any back; dots-only also matches no tone, so
its tone error is the worst. Copy-hops is sharper than copy and worse in mean error, as bench 7
saw (streaks deep in wide masks).

**Manga Cleaner Reproduction's own examples, by eye** (3x crops of each version over its raw answer):
its answers already carry the page's screen in phase, so on the screentone example every version
looks alike and copy is as crisp as the page. On its line-art example our copy and redraw add
faint grey specks and one dark smudge to white paper near hatching, which Poisson and
dots-only do not: the screen step takes hatching next to the mask for a screen.

**Hatching fix (2026-10-27).** The hatching's region fitted a screen model at 0.507, just over
`MIN_FIT` 0.5, with two frequencies from leaves hatched in different directions. In Manga Cleaner
Reproduction, the raster step declines there because almost no page pixel shows both frequencies at once
(`_screen_evidence`). `halftone::Screen::fit` now applies the same kind of test (5 px window,
detail over `MIN_DETAIL`, 35 % of it at the pair and 6 % at each) and needs 256 such pixels. Its
floor and count differ from that code: 2 levels RMS of this module's detail and 256 pixels
per region, where the original uses power 36 of a 2 px high-pass, 256 pixels per crop and 128
per region. Since the 2026-10-27 review it tries every frequency pair that fits at `MIN_FIT` or
better, best fit first, and takes the first the page shows both of; before, only the best-fit
pair was tested, so a hatching pair that fitted better hid a real screen (unit test: hatching at
0.73 beside a faint screen at 0.72, and the screen is still found). The counts and the table
below were taken before that change. On the
line-art example the region has 0, the screentone example's has 44180 of 67301. Inside the mask,
against the ported Poisson output, with pixels where Poisson is white paper (235 or more):

| finishing | mae | paper px 12+ darker | paper px 40+ darker |
|---|---|---|---|
| copy, redraw before | 3.60 | 181 | 56 |
| copy, redraw after | 1.56 | 0 | 0 |
| tone-only (for scale) | 1.56 | 0 | 0 |

Copy and redraw on the screentone example are byte for byte unchanged. Unit tests: hatching in
two directions beside a mask (fits 0.69, no evidence) gets no screen, and a dot screen beside
hatched paper in the same region is still copied. The synthetic-truth bench was not rerun: its
sets and bf16 answers were lost with old scratchpads (docs/findings.md, What has not been
measured). So every copy and redraw number in benches 6 to 9, the table above and bench 9's
copy columns included, predates this fix.

The generation switches are covered by worker tests with a fake pipeline and by a shared request
fixture. Bench 9, below, rendered them on a GPU on 2026-10-26, before the hatching fix.

## Bench 9 (2026-10-26): the generation switches on a GPU

The current worker code (`Flux3EditRunner`, 16 steps, seed 1) on the official bf16 snapshot, in
a throwaway Modal app on the aizendazai46 profile (three L40S containers, about $1.49; the app
and its weights volume are deleted). The live install there is Qwen, so it was not redeployed:
the switches only act on FLUX 3. Each arm changes only its `flux3_generation`. A kept marker
was recorded instead of refused, so every arm returned an image.

**Fake lettering** (mean of 15 halftone crops; mae / tone / screen / sharp; chapter pages are
16 crops, mae only):

| arm | raw, halftone | raw, chapter | copy finishing, halftone | copy, chapter |
|---|---|---|---|---|
| own (no switch) | 28.4 / 9.3 / 0.55 / 0.98 | 34.2 | 18.3 / 6.8 / 0.76 / 1.01 | 31.6 |
| prompt | 27.3 / 9.7 / 0.59 / 1.04 | 33.7 | 17.8 / 6.4 / 0.76 / 0.99 | 31.4 |
| marker | 39.4 / 12.6 / 0.31 / 0.99 | 39.4 | 24.5 / 10.1 / 0.64 / 1.04 | 36.6 |
| shift | 27.9 / 9.2 / 0.57 / 1.00 | 34.4 | 18.3 / 6.9 / 0.76 / 1.00 | 31.8 |
| megapixel size | 33.7 / 10.9 / 0.49 / 1.11 | 29.3 | 18.2 / 6.6 / 0.77 / 1.03 | 26.1 |
| all four | 43.8 / 13.6 / 0.25 / 1.10 | 41.4 | 24.7 / 11.0 / 0.65 / 1.05 | 37.8 |

- **Marker hurts.** It is worse on 14 to 15 of 15 halftone crops and 15 of 16 chapter crops
  (median +6 to +9.5 mae). By eye (4x zooms) the model redraws the covered area from nothing: it
  moves contours (a hair edge shifts several pixels) and draws a wavy moiré in the dots. Our own
  hint keeps a faint view of the page under the lettering; the marker takes that away. The
  residue check never fired: 0 of 96 marker renders kept any magenta.
- **Prompt and shift are noise.** After copy finishing both are within 0.5 mae of own. Raw, shift
  is within 0.5 too, and the prompt is 1.1 lower on halftone (27.3 against 28.4) and 0.5 lower on
  the chapter pages; copy finishing takes that gap to 0.5.
- **Megapixel size is mixed.** On halftone it is worse raw (+2.4 median) and equal after copy
  finishing. On chapter pages the mean drops by 5.5, but crop by crop it is better on 7 and
  worse on 9 (median +0.6): a few large crops carry the mean. It takes about twice the time
  (11.6 s against 6.2 s per crop) and 22.8 GiB peak against 21.1.
- **All four together** carries the marker's loss and is the worst arm.

**Real lettering, 17 crops from chapters 182 to 187** (own, marker, all; no truth, 3x zooms of six):
every arm removes the lettering cleanly on white bubbles, and the three look alike there. On one
crop (182_04) "all" kept a small dark piece of art at the bubble edge that own and marker painted
white. Time per crop: own 4.9 s, marker 5.0 s, all 11.5 s. Prompt, shift and megapixel size on
their own were not run on real lettering, and every arm of bench 9 used seed 1 only.

Verdict: keep the defaults (all switches off). The switches stay as settings. Do not turn on the
marker for FLUX 3 on our hints. Megapixel size is the only one worth a second look, on larger
chapter crops.

## Finishing for Qwen, FLUX.2 Klein and LaMa (2026-10-27): a setting, off by default

Bench 6 found the 3.0.0 post-processing better on Qwen's answers (halftone 31.2 to 17.5) and on
LaMa's (15.7 to 14.5). Settings > Post-processing now has **Also finish Qwen, FLUX.2 Klein and
LaMa results** (`finishingEveryModel`). Off, nothing changes: a 2.0.0 answer keeps its tone alignment and so
does each LaMa tile. On, the picked finishing (the default copy too) takes the place of that
alignment:
- Every cloud recipe that is not 3.0.0: Qwen and both FLUX.2 Klein recipes (4B and 9B) are 2.0.0.
  The recipe carries the choice outside the request digest (`cloud_clean::apply_finishing`), the
  default included, where a 3.0.0 recipe carries one only when it is not the default; and
  `PreparedRender::composite` finishes the V2 crop as it finishes a V3 one. The request does not
  change, so no worker is redeployed.
- LaMa: each tile's answer is finished on the tile as the model saw it, over the pixels that
  tile writes (`Inpainter::render_with`), as bench 6's L1 arm did. Page runs and hand edits both
  read the setting; the patch records the report under `postprocess`, stamped
  `postprocess_version` (3.0.0). A malformed finishing setting stops a run only when the switch
  is on; a hand edit then finishes nothing.

Not measured: the shipped copy step on Qwen and LaMa answers (bench 6 used bench 5's redraw),
Qwen's V2 crop (128 px wider a side than the 448 px crops bench 6 used), regions large enough
for LaMa to tile, and FLUX.2 Klein answers through any finishing (never benched). The synthetic
sets and their answers were in session scratchpads that are gone.

## Review fixes to finishing (2026-10-27): synthetic probes only

A review of the code above found these and they are fixed. Every check is a unit test or a
synthetic probe; nothing ran on a real chapter or a GPU.

- **LaMa tiles read lettering as page.** A tile's page evidence included the hand-off band and
  pixels earlier tiles had not written yet, where the lettering still is. It is now page, opaque,
  and either written by this tile or outside the applied mask, as V2's ring already treats them.
  On a synthetic 3-tile region (flat paper, solid lettering), the error off paper went from 0.87
  mean (worst 11) to 0.57 (worst 3) with Copy, and from 23.11 (worst 213) to 0.47 (worst 2) with
  Local tone match; the tone-aligned render is 0.67. On fixture pages with 4-tile 700 px
  regions and the default finishing, the change moved the patch by 16.8 levels on page-sfx, 8.8
  on page-crowded and 4.1 on page-screentone; there is no truth there, so that is the size of the
  change, not which is better.
- **LaMa finishing kept 8 bits.** It now finishes from the model's float answer and the page's
  float pixels and writes the float result, so a 16-bit page keeps its levels.
- **Poisson pulled toward off-page pixels.** See the port check in bench 8: off-page pixels now
  take the answer as the boundary. Probe with a black off-page band 6 px from the mask: 82 levels
  off paper, now 0. In the LaMa tile probe, Blend edges went from 114 off to 0 and
  Local tone match from 91 to 0 (the tile fix alone does the second).
- **Pixels outside the mask changed.** `postprocess::settle_with` now keeps the answer outside the
  mask for every finishing. Before, Simple tone match changed 7513 such pixels in a probe,
  Local tone match and Blend edges 11, and our own finishings some; now 0 for all 11.
- **Screen evidence tried one pair.** See the hatching fix in bench 8.
- **Speed.** The shared Gaussian blur runs on every core with precomputed edge indices; results
  are bit for bit the old ones (49 size and sigma pairs hashed), and `settle_with` gave bit for bit
  the old output in 528 cases before the outside-mask change. Times at opt-level 2 on a machine
  loaded by other sessions (load average 16 to 42), so read them as upper bounds:

| Case | Before | After |
|---|---|---|
| blur 2048 x 2048, sigma 2 / 16 / 48 | 0.14 / 0.95 / 2.69 s | 0.02 / 0.18 / 0.65 s |
| 120 x 60 mask, 1024 crop, Copy / tone only | 5.9 / 4.3 s | 1.4 / 0.9 s |
| 120 x 60 mask, 2048 crop, Copy / tone only | 18.3 / 11.9 s | 5.2 / 4.9 s |
| 1200 x 1200 mask, 2048 crop, Copy, grey / colour | 72.6 / 60.5 s | 24.4 / 16.3 s |
| 1200 x 1200 mask, 2048 crop, Blend edges, grey / colour | not measured | 2.9 / 6.3 s |

Still open: our own finishings read off-page pixels through the low-pass in their tone and
lighting steps (13.4 levels off in the off-page band probe; a masked blur over the page would fix
it and changes results on pages with transparency); grey Copy on a 1200 px mask still takes about
24 s, mostly the halftone phase and demodulation blurs over the whole window; and V2's tone
measure (`ToneSite`) still reads 8-bit page samples.

