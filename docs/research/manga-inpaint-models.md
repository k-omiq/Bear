# Manga text removal models: research and the masked Klein recipe

Written 2026-10-01 on `codex/cloud-integration`. Everything below was run on one page:
`~/Downloads/test inno/denoised/08-058.png` (654 by 919, black and white with screentone, royalty free),
12 regions: 7 balloons from the app's own detection and 5 sound effects drawn over art. The app's
bubble detector does not see the sound effects; their masks are the SAM-TS-L text mask
(`spikes/sam-ts-l/infer_ort.py --provider cpu`) inside a box per sound effect, taken through
`spike-cloud-crops export` with `MASKS_DIR`, the way a brushed mask is (Benchmark 3 redoes this
with the app's own padding).

One page is a small sample. The scores compare variants on these 12 regions; they are not general.

## The question

Qwen-Image-Edit-2511 works on colour pages and goes wrong on black-and-white manga; FLUX.2 Klein
edited the whole crop; LaMa (`dreMaz/AnimeMangaInpainting`) fails on large regions and screentone.
Is there a model trained for manga text removal to use instead?

## What exists (web research, 2026-10-01)

No open model newer than MangaInpainting (SIGGRAPH 2021) is trained for black-and-white manga text
removal. Candidates found:

| Candidate | What it is | Licence |
|---|---|---|
| `msxie92/MangaInpainting` (IOPaint ships `manga_inpaintor.jit`) | Screentone-aware CNN, Manga109, grayscale only | academic and commercial |
| `tori29umai/QwenImageEdit2511_LoRA`, `QIE2511_FukidashiErase_V1` | Qwen 2511 LoRA: removes balloon text and the balloon itself | Apache-2.0 |
| `SakikoLab/Anime-Image-Purifier-Kontext-LoRA-v2` | FLUX.1 Kontext LoRA, 21,920 anime pairs, text and watermark removal | Apache-2.0 (base FLUX dev NC) |
| `ShinoharaHare/Waifu-Inpaint-XL`, `Acly/NoobAI-Inpainting` | Anime SDXL inpaint bases, no manga evidence | OpenRAIL++ / FAIPL |
| `mayocream/RORem-mixed-GGUF` (Koharu) | SDXL object remover, not manga trained | Apache-2.0 + OpenRAIL++ |

The one published benchmark on black-and-white manga text removal (Furuta, arXiv 2603.27790v2,
Manga109s, mask free, "Remove text. Keep everything else unchanged.") put Klein 9B at 26.9 PSNR and
Qwen 2511 at 21.2. Scanlation tools that use diffusion (Koharu, MangaTranslator, ImageTrans,
BallonsTranslator) use Klein or a LaMa finetune, not Qwen.

## Benchmark 1: three candidates, one L40S each

`ink_left` is the share of dark pixels inside the write mask still dark after the composite (lower
is better, except on a black balloon, where the balloon itself is dark). `out_diff` is how much the
raw answer changed the crop outside the write mask, 0 to 255.

| Variant | s/crop | out_diff | ink_left | By eye |
|---|---|---|---|---|
| Klein 9B, no mask, "Remove all text." (the old recipe) | 2.8 | 16.1 | 0.28 | clean |
| **Klein 9B, mask, crop as reference, MangaTranslator prompt** | 3.9 | 5.3 | **0.21** | clean balloons, black balloon stays black, sound effects removed |
| Klein 9B, mask, no reference | 2.8 | 5.3 | 0.26 | drew a new black stroke into one sound effect |
| Klein 9B, mask, "Remove all text." | 3.9 | 5.3 | 0.35 | left more lettering |
| Qwen 2511 production (4-bit, Lightning, `QWEN_PROMPT`) | 8.5 | 39.2 | 0.12 | **peach and orange blobs on a grey page**; black balloon painted white |
| Qwen 2511 + FukidashiErase, "keep the speech bubbles" prompt | 8.5 | 9.4 | 0.11 | no colour; black balloon painted white |
| Qwen 2511 + FukidashiErase, its own prompt | 8.5 | 21.9 | 0.04 | grey art blobs inside white balloons (it erases the balloon) |
| Kontext + Purifier LoRA (edit and masked) | 20.7 | 22.8 / 4.9 | 0.86 / 0.94 | removed almost nothing |
| Kontext, mask, MangaTranslator prompt, no LoRA | 14.0 | 5.0 | 0.28 | balloons clean, sound effects partly left |

The Purifier LoRA could not load through `load_lora_weights` on the SDNQ Kontext base (shape check
on packed 4-bit weights) and was attached with `load_lora_into_transformer`; whether any keys
matched is not checked. Spend: about $1.60 across three Modal workspaces.

## Benchmark 2: the production runner, Klein 4B and 9B

`SdnqFluxRunner` as committed (`Flux2KleinInpaintPipeline`, hole = hint grown by `HOLE_GROWTH`,
crop as reference, MangaTranslator prompt) on an L40S, same 12 regions.

| Variant | s/crop | out_diff | ink_left |
|---|---|---|---|
| 9B old recipe | 2.8 | 16.1 | 0.28 |
| **9B masked (production)** | 3.9 | 5.3 | 0.21 |
| 4B old recipe | 1.2 | 14.9 | 0.16 |
| **4B masked (production)** | 1.9 | 5.3 | 0.30 |
| 4B masked, no reference | 1.9 | 5.3 | 0.28 |
| 4B masked, "Remove all text." | 1.9 | 5.3 | 0.21 |

On 4B the numbers favour the old prompt, and the eye does not: with "Remove all text." 4B left the
black balloon's text and a grey stroke in a sound effect; with the MangaTranslator prompt it did
neither. The old 4B recipe's low `ink_left` hides a scribble it drew inside a white balloon. One
prompt for both, the MangaTranslator one. Peak VRAM 13.2 GiB (9B), 6.5 GiB (4B).

**Noise in the hole, rejected.** A suggestion for colour anime models' "flat colour bias": add 5
to 10% random noise to the masked area before generation. At `strength=1.0` the hole already starts
from pure noise, so the only thing it can change is the reference image, whose lettering it
partly hides. Gaussian noise of 5% and 10% of full scale inside the hole of the image and the
reference: 9B `ink_left` 0.20 against 0.21, `out_diff` 5.5 to 5.7 against 5.3; on 4B at 10% the black
balloon's text came back. Not adopted.

## Local mflux run (Apple Silicon, 32 GB Mac)

Klein 4B through the sidecar's `_generate_masked`, with the app's MLX caps (`set_cache_limit(1e9)`,
`set_memory_limit(12e9)`), 4 steps, same regions.

| Variant | s/crop | MLX peak | out_diff | ink_left |
|---|---|---|---|---|
| Old edit, "Remove all text.", `generate_image` (2 crops) | 35 to 49 | 7.8 to 8.4 GiB | 15.7 | 0.03 |
| Masked, soft mask, first version (12 crops) | 42 to 84 | 11.2 to 12.2 GiB | 5.55 | 0.22 |
| **Masked, dtype fix (shipped)** (12 crops, app padded masks) | 16 to 49 | 7.5 to 9.1 GiB | 5.45 | 0.23 |

The first version was slow because the hold mixed in the float32 sigmas, so steps 2 to 4 ran the
transformer in float32; the latents are now cast back to their own dtype after the hold, as
mflux's own scheduler step does. Same session, peak reset per crop: p08_00 27.9 s and 11.2 GiB
before, 16.5 s and 7.8 GiB after, against 14.1 to 16.4 s and 7.79 GiB for `generate_image`. The
cast moves the answer by a mean of 0.34 to 0.56 grey levels. KV caching is not the difference:
mflux 0.18.1 enables it only for the 9B KV model, so the 4B edit never used it. Times climbed
across the 12 crop run while the peak stayed flat; this Mac's load during the run is not known.
Both local runs paint the black "だる……" balloon white; the cloud 4B masked run keeps it black. The
mask does not cause this; the MLX 4B weights or its sampling do.

## Benchmark 3: the app's own padding on the sound-effect masks

Benchmarks 1 and 2 took the SAM masks as a brush stroke (`manual`), so the hint was the bare SAM
pixels. A Detect run does more with SAM lettering outside a balloon (`run.rs`): `fit::outlined`
adds the outline an effect draws round its letters, then the detecting fit grows it
(`fit_within(..., manual=false)`). `spike-cloud-crops` now does the same with `MASKS_AS=detect`;
the 5 sound-effect hints grew 2.3 to 3.7 times in area. The 7 balloon crops already came from the
app's fit (one changed slightly with the current detector).

| Variant | s/crop | out_diff | ink_left | ink_left_sfx |
|---|---|---|---|---|
| Klein 9B masked | 3.9 | 5.22 | 0.23 | 0.29 |
| Klein 4B masked | 1.9 | 5.20 | 0.29 | 0.28 |
| Qwen v2 | 8.4 | 9.79 | 0.20 | 0.44 |
| Klein 4B local (mflux) | 16 to 49 | 5.45 | 0.23 | 0.30 |

`ink_left` is not comparable to the tables above: the wider write mask now takes in speed lines and
other art, which should stay. By eye the padding removed the outlines and ghosts the bare SAM masks
left (9B on "ンッ" most clearly). Klein 9B cleaned all five sound effects; Qwen left one ("バ")
whole. Klein 4B drew a new grey stroke into "ンッ" in the cloud and a short dark stroke into
"ツシャ" locally.

## Benchmark 4: the manga LaMa, two exports

`dreMaz/AnimeMangaInpainting` (`lama_large_512px.ckpt`, big LaMa finetuned on 300k manga and
anime images, MIT) is the model the app already ships: `lama-manga.onnx` is
`mayocream/lama-manga-onnx`'s export of it. `TareHimself/AnimeMangaInpainting-torchscript`
(@ `b592884`) is a TorchScript export of the same checkpoint that takes any size divisible by 8.
Both ran on Modal CPU (8 cores) on the Benchmark 3 crops, mask = the write region (alpha > 0). The
ONNX graph is fixed at 512 by 512, so crops were edge-padded to 512 and the two over 512 scaled
down first (the app tiles them instead).

| Variant | s/crop (CPU) | out_diff | ink_left | ink_left_sfx |
|---|---|---|---|---|
| `lama-manga.onnx` | 3.1 to 3.7 | 3.22 | 0.20 | 0.25 |
| TorchScript | 0.8 to 1.9 | 0.00 | 0.20 | 0.25 |

The two answers differ by a mean of 1.7 to 2.6 grey levels on crops up to 512 and 7.9 to 8.3 on the
two scaled ones, mostly outside the hole: the TorchScript graph returns the known pixels unchanged,
the ONNX one moves them (`lama.rs` documents this), and the composite discards them either way. No
visible quality difference. With the app's padded masks LaMa did better than its reputation here:
it kept the black balloon black (the only model besides Klein to do so) and removed every sound
effect. It is still weaker than Klein 9B where the hole crosses texture: it smears speed lines into
grey and left a light streak across the screentone under "ズバババ", where 9B continued the tone.

## What was built

- Cloud: `deploy/cloud/common/flux.py` `SdnqFluxRunner` renders through `Flux2KleinInpaintPipeline`
  with the hint grown by `HOLE_GROWTH` (8 px) as the mask and the crop as `image_reference`;
  `mask_image`, `image_reference` and `strength=1.0` are passed unfiltered. Recipe ids are now
  `mc-flux2-klein-inpaint-v1` and `mc-flux2-klein-9b-inpaint-v1`. `native_mask_conditioning` stays
  false: the model takes no mask channel; the pipeline holds the latents outside the hole.
- Local sdnq backend: the same pipeline and call (`backend/sdnq.py`), the hole from
  `backend/base.py` `hole_for`.
- Local mflux backend (Apple Silicon): mflux 0.18.1 has no Klein mask, so `_generate_masked` is the
  edit's loop plus diffusers' latent hold, with diffusers' mask weighting (the centre 2 by 2 pixels
  of each 16 px block).
- Qwen recipe v2 (`mc-qwen-image-edit-2511-v2`): `QwenEditRunner` loads FukidashiErase beside
  Lightning, both active at 1.0, and `QWEN_PROMPT` is the "keep the speech bubbles" wording. The seed
  downloads both LoRAs into `<snapshot>/adapters`; the marker's file count and size change, so a
  volume seeded for v1 seeds again (only the new 18.8 MB file downloads). Checked on an L40S through
  the production runner: 12 of 12, 8.4 s/crop, peak VRAM 21.45 GiB (the pinned 22,016 MiB still
  holds), `out_diff` 9.35, `ink_left` 0.11, no colour; the black balloon is still painted white.
- `flux.rs` `PROMPT` is the MangaTranslator wording; `test_recipe_parity.py` holds the cloud prompt,
  call, `HOLE_GROWTH` and the unfiltered keywords equal to the local ones.

## Not measured

- Colour pages: nothing here was run on one. The new prompt names screentone and Japanese sound
  effects, the vocabulary `flux.rs` once kept out for fear of colour drift (never measured either).
- Only one page. No Beam run. No live gateway deploy of the new recipe.
- The render rate estimate (`cloud_clean.rs` `RenderRate`) still prices the edit; the masked render
  measured about 1.4 times the edit's GPU time on the L40S.
- The local sdnq backend's masked render was not run on a GPU; the mflux one was run on this Mac.
- Why local 4B paints the black balloon white and cloud 4B does not.
