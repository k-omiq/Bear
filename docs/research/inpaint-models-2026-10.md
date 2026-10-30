# Inpainting models for manga text removal: bench of 30 October 2026

The question: do the newest open inpainting models, and the two ComfyUI workflows the user brought
(`Z-image_Inpaint_v4.json`, `SD_Inpaint_Unified_v3.json`), clean manga better than the app's cloud
models? All GPU work ran on Modal profile `k-omiq`, one L40S per arm. Total spend: about $6.50,
including the seed downloads, smoke runs and the resource run.

**Answer: none of them replaces FLUX 3.** The bench found a bigger problem in FLUX 3 itself: it left
bold lettering in place on 5 of 19 real crops. Recipes `mc-flux3-klein-edit-v2` and
`mc-flux3-klein-bf16-edit-v2` detect that and edit again from a LaMa fill. That removed all five and
changed nothing on the other crops.

## The two workflows

`Z-image_Inpaint_v4`: Z-Image Turbo (bf16) with the Fun ControlNet Union 2.1 2602 8-step patch in
inpaint mode, no control image, region cropped with 128 px context and scaled to 1024, mask grown 6
and blurred 3, empty latent, denoise 1, cfg 1, 8 steps, shift 3. Three sampler routes (native,
Capitan01R, ClownsharK). A FLUX Fill OneReward GGUF loader sits bypassed.

`SD_Inpaint_Unified_v3`: SDXL (Juggernaut Ragnarok, DMD2 LoRA) with one of BrushNet, Fooocus inpaint,
ControlNet Union ProMax repaint, PowerPaint (SD 1.5) or the LanPaint sampler. Same region crop, 128 px
context, mask grown 32.

Both are region crop, sample, paste by mask. The app's worker and client already do that, so the only
thing worth taking from either is the model.

## What exists (web and model cards, 2026-10-30)

| Model | Route | Licence | Note |
|---|---|---|---|
| Z-Image Turbo + Fun ControlNet Union 2.1 | `ZImageControlNetInpaintPipeline` | Apache-2.0 | no official Z-Image Edit weights |
| Z-Image Turbo, plain inpaint | `ZImageInpaintPipeline` | Apache-2.0 | |
| FLUX.1 Fill + OneReward + removal LoRA | `FluxFillCFGPipeline`, OneReward's repo | CC BY-NC 4.0 | `FLUX.1-Fill-dev` is gated (403 on the saved token); its VAE and encoders came from `ostris/Flex.1-alpha` |
| Qwen-Image 2.1 Turbo | `QwenImage21Pipeline` | Qwen Research License | not Apache; no mask input in the card's example, so the hole was painted magenta |
| SDXL + ControlNet Union ProMax | `StableDiffusionXLControlNetUnionInpaintPipeline` | Apache-2.0 (ControlNet), CC BY-NC-ND (Juggernaut XI) | Juggernaut XI stands in for Ragnarok, which is not on the Hub |
| RORem | `StableDiffusionXLInpaintPipeline` | Apache-2.0 | SDXL trained for object removal |
| Moebius (hustvl) | own code, diffusers 0.38 | MIT or Apache (card and README differ) | 0.2B, checkpoints tuned on Places2, CelebA-HQ, FFHQ |
| LanPaint | ComfyUI node, `LanPaint-diffusers` port | GPL-3.0 | a sampler for any model, up to 5 times slower |

## Set and scores

31 crops, seed 1, working long side 1024 for the new models, the recipe's own size for the app's:
- 19 real regions from `test_pages_5px`: the biggest inpaint-route region of each page, plus the 5
  biggest held regions. There is no truth for these.
- 12 synthetic crops: outlined katakana (Noto Sans CJK Bold) over 448 px windows of real pages, 10 of
  them from `spikes/screentone-fill/corpus/pages`. The window is the truth.

Answers were composited through `alpha.png` in Python. The app's tone alignment and finishing did not
run, so FLUX 3's numbers here are its raw answer. `mae`, `tone` (after a 2.5 px blur), `screen`
(correlation of 1.5 px detail) and `detail` (detail strength against truth) follow the synthetic truth
recipe; `chroma` is colour left on a grey page.

| Arm | mae | tone | screen | detail | chroma (real) | s/crop | By eye, real crops |
|---|---|---|---|---|---|---|---|
| **FLUX 3 bf16, v1** | **23.3** | **14.8** | **0.55** | 0.93 | 2.6 | 6.4 | best texture; **lettering left on 5 of 19** |
| **FLUX 3 bf16, v2 (this change)** | **23.3** | **14.8** | **0.55** | 0.93 | 3.8 | 8.1 | all 19 clean |
| FLUX 3 on a LaMa fill, always | 32.3 | 23.3 | 0.43 | 0.84 | 3.9 | 6.8 | all clean, invents more |
| FLUX 3 with the magenta marker | 37.8 | 24.2 | 0.29 | 1.02 | 3.9 | 6.4 | all clean, redraws art |
| Klein 9B v2 (LaMa fill) | 29.0 | 19.6 | 0.46 | 0.73 | 3.5 | 3.9 | all clean, soft |
| LaMa alone | 28.9 | 19.7 | 0.45 | 0.84 | 1.7 | 0.4 | clean, smeared on big holes |
| Qwen 2.1 Turbo, marker | 32.2 | 18.8 | 0.32 | 0.90 | 3.5 | 4.4 | clean, faint grey stains in white balloons |
| Qwen 2.1 Turbo, LaMa fill | 31.8 | 20.5 | 0.40 | 0.94 | 3.5 | 4.7 | same stains |
| Z-Image ControlNet, scale 0.9 | 29.8 | 19.8 | 0.45 | 0.75 | 5.3 | 5.3 | LaMa with a brown tint |
| Z-Image ControlNet, scale 0.65 | 34.1 | 22.5 | 0.44 | 0.66 | 5.8 | 5.1 | tint, drew fake text in a balloon |
| OneReward + LoRA, 30 steps | 29.6 | 18.6 | 0.41 | 0.85 | 1.5 | 35.3 | **drew fake text in balloons, redrew bold strokes** |
| OneReward on a LaMa fill | 29.8 | 18.8 | 0.41 | 0.85 | 1.5 | 34.9 | same, the fill changes nothing |
| SDXL Union ProMax | 35.7 | 23.8 | 0.34 | 1.01 | 1.5 | 3.8 | grey blotches on black panels |
| Z-Image inpaint, strength 0.6 | 33.7 | 21.8 | 0.36 | 0.91 | 1.7 | 2.3 | draws ghost figures and ornaments |
| RORem | 42.5 | 28.9 | 0.28 | 0.69 | 2.6 | 2.7 | dark streaks |
| Z-Image inpaint, strength 1.0 | 97.7 | 79.1 | 0.06 | 1.43 | 1.9 | 3.4 | **draws whole people into the hole** |

The first prompt tried on Z-Image inpaint, which said "clean and blank", painted the holes flat white
instead. A strength-1.0 Turbo has no idea the hole should stay empty: a distilled model with no CFG
follows its prior, and that prior is a person.

## Resources

Six crops per model on one L40S (48 GB), measured with `nvidia-smi` every 0.5 s and the process's
peak RSS. GPU peak includes the CUDA context and torch's cache. Download is what the model needs on
disk. Time is this small set, which holds the biggest crops, so it differs from the table above.

| Model | GPU peak | Torch peak | Host RAM peak | s/crop | Load | Download |
|---|---|---|---|---|---|---|
| LaMa alone | 1.5 GiB | 0 | 6.8 GiB | 1.2 | under 1 s | 0.2 GB |
| RORem | 11.8 GiB | 9.0 GiB | 16.5 GiB | 2.8 | 17 s | 6.9 GB |
| Klein 9B v2 (4 bit, with LaMa) | 18.1 GiB | 13.9 GiB | 17.4 GiB | 5.0 | 33 s | 12.8 GB |
| SDXL Union ProMax (with LaMa) | 18.5 GiB | 11.7 GiB | 24.2 GiB | 4.5 | 28 s | 16.4 GB |
| Z-Image inpaint (with LaMa) | 25.7 GiB | 21.7 GiB | 31.5 GiB | 4.1 | 23 s | 32.8 GB |
| **FLUX 3 bf16 v1** | 29.5 GiB | 22.3 GiB | 24.4 GiB | 9.4 | 28 s | 20.3 GB |
| **FLUX 3 bf16 v2 (with LaMa, 31 crops)** | 34.3 GiB | 23.5 GiB | 24.4 GiB | 8.1 | 36 s | 20.5 GB |
| Z-Image ControlNet (with LaMa) | 33.9 GiB | 28.4 GiB | 33.8 GiB | 5.3 | 20 s | 39.5 GB |
| OneReward + LoRA | 36.8 GiB | 34.0 GiB | 36.8 GiB | 30.3 | 37 s | 34.0 GB |
| Qwen 2.1 Turbo | 42.2 GiB | 38.8 GiB | 35.7 GiB | 4.4 | 36 s | 32.5 GB |

## The change

On the 31 crops, the correlation of the lettering's grey pixels before and after FLUX 3's edit split
the failures from the rest with nothing between them:

- the 5 failed crops: 0.77, 0.79, 0.995, 0.997, 0.997;
- every other crop: 0.21 or less.

The Qwen edit ratio (`MIN_EDIT_RATIO`, 2.0) would have missed one of them, at 3.69, and caught another
only just, at 1.91. So `flux3.lettering_kept` uses correlation, with the limit at 0.5, and a kept crop
is filled by LaMa and edited again at the same seed. Result, with the real runner on all 31 crops: all
five clean by eye, the 12 synthetic scores the same as v1 to three places, and a failed crop takes about
twice as long (mean over all crops 6.4 to 8.1 s).

The marker path keeps its own check. The LaMa graph is now seeded beside both FLUX 3 snapshots (207 MB),
so an update reseeds that one file. The worker image gains `onnxruntime-gpu`.

The check saw no false alarm here, but only 31 crops back it. Bold lettering a correlation of 0.5 reads
as gone, or art that stays correlated under a real removal, would be missed or edited twice; neither
appeared.

## Real-crop numbers

The 19 real crops have no truth, so these only compare arms. `ink_left` is the share of pixels dark in
the input (below 90) and still dark after the composite, inside the write mask; dark art under the
lettering keeps it above 0. `outside` is the mean change of the raw answer outside the write mask, 0 to
255, which the client's composite does not write.

| Arm | ink_left | outside |
|---|---|---|
| FLUX 3 bf16 v1 | 0.457 | 10.0 |
| FLUX 3 bf16 v2 | 0.297 | 10.0 |
| FLUX 3 on a LaMa fill | 0.277 | 11.1 |
| FLUX 3 with the marker | 0.286 | 9.1 |
| Klein 9B v2 | 0.282 | 7.7 |
| LaMa alone | 0.291 | 0.5 |
| Qwen 2.1 Turbo, marker | 0.254 | 33.6 |
| Qwen 2.1 Turbo, LaMa fill | 0.265 | 34.6 |
| Z-Image ControlNet, 0.9 | 0.262 | 16.5 |
| Z-Image ControlNet, 0.65 | 0.246 | 26.3 |
| OneReward + LoRA | 0.332 | 9.3 |
| OneReward on a LaMa fill | 0.314 | 9.3 |
| SDXL Union ProMax | 0.255 | 12.4 |
| Z-Image inpaint, 0.6 | 0.279 | 7.0 |
| Z-Image inpaint, 1.0 | 0.225 | 8.5 |
| RORem | 0.265 | 13.2 |

## The five crops v2 changed

`correlation` is the check on FLUX 3's first answer. `ink_left` is for that crop, v1 then v2.
`real_6_00` and `real_14_00` stay high in v2 because dark art lies under the lettering; both look clean.

| Crop | correlation | ink_left v1 | ink_left v2 |
|---|---|---|---|
| `real_3_00` | 0.770 | 0.740 | 0.145 |
| `real_6_00` | 0.997 | 0.990 | 0.520 |
| `real_14_01` | 0.995 | 0.966 | 0.193 |
| `real_14_00` | 0.997 | 0.972 | 0.586 |
| `real_1_08` | 0.787 | 0.871 | 0.070 |

The other 26 crops read between -0.068 and 0.208.

## Where the bench differs from the workflows and the model cards

- It ran the models through Diffusers, not ComfyUI, so the Capitan01R and ClownsharK samplers were not
  tested.
- The Z-Image workflow has no control image. Diffusers needs one, so the control image was the LaMa fill
  in grey. The mask was grown 8 px by the worker's rule, not 6 with a blur of 3.
- OneReward ran 30 steps with `true_cfg` 4.0; its card uses 50.
- SDXL ran without the workflow's DMD2 LoRA, PAG or Sage attention: 20 steps, guidance 5.0.
- RORem ran 30 steps at guidance 1.0 with an empty prompt.
- The bench image pinned Diffusers 0.42.1; the FLUX 3 worker pins 0.42.0.
- Planned and never run: the lite Z-Image ControlNet, and Z-Image inpaint without a LaMa fill.
- The Z-Image prompt was changed once, after the first one painted the holes white. The same prompt then
  served Z-Image and SDXL.

## FLUX 3 v2, the code

Commit `2f75dd0`, merged into `main` on 2026-10-30. Not deployed: each FLUX 3 profile moves when its owner updates it.

- `deploy/cloud/common/flux3.py`: `lettering_kept`, `LETTERING_KEPT_CORRELATION` 0.5, `_edit` split out of
  `render_png`, and the LaMa session loaded with the pipeline.
- `deploy/cloud/common/manifest.py`: recipes renamed to v2, `LAMA_ADAPTER` on both FLUX 3 models.
- `deploy/cloud/common/contract.py`, `crates/cleaner-core/src/engines/render.rs`, `src/lib/editor/qwenflow.js`:
  the recipe names in their allowed lists.
- `deploy/cloud/modal/app.py`, `deploy/cloud/beam/app.py`: `onnxruntime-gpu==1.23.2` in the FLUX 3 image.
- Fixtures and tests: new request digests, and 6 new tests in `deploy/cloud/tests/test_flux3.py`.

What changes for a user: an older app refuses a v2 gateway, so the app and the cloud setup move together.
A second render uses the same GPU job and the same seed. The 4 bit recipe's host memory with LaMa was not
measured; its `worker_memory_gib` stays 24.

Tests, all in Modal CPU containers: Python 480 cloud and 189 provisioner passed. Rust: 23 `cloud_wire`, 33
`engines::render` and 66 `inference::service` passed. `inference::cloud_clean` passed 61 of 62:
`batch_renders_keep_qwen_guidance_bound_to_their_region_grants` hit the 16 open proposal limit in the
parallel run and passes alone; the untouched base fails 2 in that same group. JS: 895 passed in
`src/lib/editor` and the inference settings test. The full Rust and JS suites did not run.

## What is saved

`~/dev/manga-bench-sets/inpaint-2026-10`, outside the repo because the crops are copyrighted:
`out/<arm>/` holds all 16 sets of answers and their `_run.json`, `set/` the 31 crops, `bench.py` and
`testapp.py` the harness and test runner, and `vendor/` OneReward's pipeline file.

`score/` covers only the last report: LaMa, FLUX 3 v1, v2 and fill, and Klein 9B. A later report overwrote
the sheets and per-crop scores of the other 11 arms, so this file holds their averages. Upload `out/` and
`set/` to a new volume and run `bench.py::report` with every arm name to rebuild them; no GPU is needed.
`_run.json` of the resource run was not kept; its numbers are the table above. The Modal volume is deleted.

## ComfyUI in the app: not built

ComfyUI is GPL-3.0, as this app is. The plan the user was shown: the app connects to a ComfyUI server the
user runs, sends the crop and mask through its web API, and composites the answer as usual. Running ComfyUI
on Modal for the user was the bigger option. Open points: workflows must be saved in API format with their
image and mask nodes marked, a missing custom node needs a clear error, custom nodes can run any code, and
pages must not go to a non-local address without consent. The user has not chosen yet.

## Pick up here

1. Update FLUX 3 profiles.
2. Try v2 on the user's own pages through the real app, with finishing on.
3. Decide on ComfyUI.
4. Decide whether LanPaint may run, with its code in the bench image.
5. Rebuild the lost sheets if the 11 other arms need another look.

## Not measured

- LanPaint. Running its Diffusers port meant cloning third-party code into the bench image, and the
  session's safety check refused that. It stays open; the app is GPL-3.0, so the licence would allow it.
- BrushNet, PowerPaint, Fooocus inpaint. These need ComfyUI nodes or custom code, date from 2023 to 2024,
  and SDXL Union ProMax, the same generation, came in well behind FLUX 3.
- Moebius. Its checkpoints are tuned on photos and faces, not line art, and it needs its own environment.
- FLUX.1 Fill without OneReward: the repository is gated, and accepting its licence is the user's call.
- Other seeds, and the app's finishing on these answers: every arm is its raw answer at seed 1.
- v2 in the deployed app: no live profile was updated.
