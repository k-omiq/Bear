# Page denoise combos, November 2026

Question (2026-11-14): which denoise models and chains do manga and comic upscalers use now, and which
should replace the app's page denoise presets. First step was test and exploration only; no app code changed.

## Pages

- Round 1: `sono akuyaku kizoku mama` chapters 49 and 50, the typeset "done" pages (1672x2400, 35 pages).
  Chapter 50 pages 2 and 3 are one colour spread; they were joined before denoising (page 3 on the left,
  page 2 on the right, right to left).
- Round 2: the same chapters' cleaned pages (`第49話_cleaned`, `第50話_cleaned`, 836x1200, 33 pages after the
  join). Colour pages: 50/001 and the 50/002+003 spread. 49/001 is B&W with a small pink logo.

Both chapters were already clean: grain in flat areas was 0.16 grey levels on the source. A noisy raw scan
has not been tried (see `findings.md`, What has not been measured).

## Research: what people use

The research pass (web, 2026-11-14) found no B&W model newer than MangaJaNai V1. New work is on colour:
IllustrationJaNai V2 (2025-07) and V3 (2025-11) on the FDAT architecture, which spandrel loads from 0.4.2.
umzi's reline tool chains descreen, then new screentone, then resize. MangaJaNaiConverterGui's default
workflow sends grey pages to MangaJaNai by page height and colour pages to IllustrationJaNai V3denoise; it
runs no separate dejpeg step. Reddit and Discord threads could not be read.

## Models tried

App models (already pinned in `deploy/cloud/common/denoise.py`): Real-CUGAN 2x conservative, MangaJaNai 2x
auto, waifu2x art_scan 4x noise2, MangaJPEG HQ.

New models (hashes as downloaded; none pinned in the app yet):

| Key | File | Arch, scale, channels | Licence | SHA-256 |
|---|---|---|---|---|
| ijanai-v3dn-2x | `2x_IllustrationJaNai_V3denoise_FDAT_M_unshuffle_30k_fp16.safetensors` in `MangaJaNai/releases/download/3.0.0/IllustrationJaNai_V3denoise.zip` | FDAT 2x RGB | CC-BY-NC-SA-4.0 | `960c9555f62fb19113e8d808bde5a54b64181b1db417bca1bbda26516e48f108` |
| ijanai-v3dn-4x | `4x_IllustrationJaNai_V3denoise_FDAT_M_47k_fp16.safetensors`, same zip | FDAT 4x RGB | CC-BY-NC-SA-4.0 | `a15dfc813b1f1138ca3da06c6cbf530dd1acbff0ba0d4eb97b5c021f3facbf6c` |
| ijanai-dejpeg-1x | `1x_IllustrationJaNai-DeJPEG_FDAT_M_100k.safetensors` in `.../2.0.0/IllustrationJaNai_V2_DeJPEG_ModelsOnly.zip` | FDAT 1x RGB | CC-BY-NC-SA-4.0 | `48b4b77977ffa10d01454ba47671990568e989bb7d76f92888572734c105ebb8` |
| animesharp-v4-2x | `Kim2091/Kim2091-Models/releases/download/2x-AnimeSharpV4/2x-AnimeSharpV4_RCAN.safetensors` | RCAN 2x RGB, no fp16 | CC-BY-NC-SA-4.0 | `6470bb91d6622d6acdff81132c1a8615b961b919ce2b9a01ce993378500cfbe1` |
| dejpg-realplksr-1x | `Phhofm/models/releases/download/1xDeJPG_realplksr_otf/1xDeJPG_realplksr_otf.safetensors` | PLKSR 1x RGB, no fp16 | CC-BY-4.0 | `bfc0b9a1163bf20eb18479e1f4faa0ece1b4d5ce59759be8b843d6ef32d1890f` |
| fbcnn-color | `jiaxi-jiang/FBCNN/releases/download/v1.0/fbcnn_color.pth` | FBCNN 1x RGB | Apache-2.0 (not checked) | `8b0e4ef23d59cf7ac934a342cb31a17619e4fa4a0b3374a9d78c5174312387e8` |
| fbcnn-gray | `.../fbcnn_gray.pth` | FBCNN 1x grey | Apache-2.0 (not checked) | `7beec25b883a6c3bd7d103429b78a2371f7e85c1f5261a6e2e5d0702d9e9f977` |
| ultrasharp-v2-4x | `huggingface.co/Kim2091/UltraSharpV2/resolve/main/4x-UltraSharpV2.safetensors` | DAT 4x RGB, no fp16 | CC-BY-NC-SA-4.0 | `2b5db674aa8f3864a696dccd1d07d7cc0500fbf220f35044cd30eee9a7f971a7` |
| eula-bw-v3-1x | `models/1x_eula_digimanga_bw_v3_nc1_52k.pth` in `bucket.yor.ovh/torch_models/1x_eula_digimanga_bw_v3_nc1_52k.tar.xz` | ESRGAN 1x grey | not stated | `1b24ff5459fdecd825ed5fc404364501d247d0e10a94cc09bbb8a8e290fdeb1f` |
| ds-atdl3-4x | `models/4x_dwtp_ds_atdl3.pth` in `.../4x_dwtp_ds_atdl3.tar.xz` | ATD 4x RGB, no fp16 | not stated | `fe781308704751979f9509e7febb8704968ca2506aa73492df1eac58318618cc` |
| ds-moesr-v2-4x | `models/4x_dwtp_ds_moesr_v2.pth` in `.../4x_dwtp_ds_moesr_v2.tar.xz` | MoESR 4x RGB | not stated | `d43df68fbd6543c47e41b6dc5ca95da82794521b26108269f75ce533d100ee65` |
| ds-dwtp-esrgan5-4x | OpenModelDB bucket `4x-DWTP-ds-esrgan-5.pth` | ESRGAN 4x RGB | not stated | `1a98d50b9e71c575012f7f03d97fcc04c268d8b977fb4a3e08d0ae9dc151e773` |

`bucket.yor.ovh` sits behind Cloudflare and answers 403 to Modal's addresses; the three files were
downloaded on the Mac and put on the bench volume. An installation would need another host for them, or a
download on the desktop. The licences of the umzi files are not stated; ask before shipping them.

## Round 1 results (done pages, page size kept)

Median per crop over 70 crops (B&W combos 62, colour combos 14). Seconds are the median per 1672x2400 page
on an A10G. change: mean difference to the source. grain: mean residual in flat areas (source 0.16).
sharp: edge strength against the source.

| Combo | s/page | change | grain | sharp | Verdict |
|---|---|---|---|---|---|
| Real-CUGAN 2x conservative (app) | 2.3 | 9.5 | 0.09 | 1.52 | **chosen for B&W**: flat clean grey, crisp lines |
| MoESR v2 descreen 4x | 13.0 | 19.1 | 0.09 | 1.80 | **chosen for B&W**: same, crisper; removes screentone |
| ATD-light descreen 4x | 34.5 | 11.6 | 0.09 | 1.53 | close to MoESR, slower (no fp16) |
| DWTP descreen ESRGAN 4x | 15.9 | 15.7 | 0.10 | 1.65 | streak artefacts; drop |
| MangaJaNai 2x (app) | 4.4 | 29.5 | 0.74 | 1.69 | redraws screentone dots in smooth grey |
| MangaJPEG HQ then MangaJaNai 2x (app) | 8.3 | 29.7 | 0.73 | 1.68 | same as MangaJaNai |
| waifu2x n1 then MangaJaNai 2x | 9.2 | 30.4 | 0.58 | 1.67 | same as MangaJaNai |
| eula digimanga bw v3 1x | 10.4 | 10.2 | 0.15 | 1.41 | partial dots, blocky |
| FBCNN gray then MangaJaNai 2x | 5.6 | 28.5 | 0.40 | 1.71 | dots |
| waifu2x art_scan 4x n2 (app) | 11.4 | 3.1 | 0.04 | 0.98 | clean but soft |
| MangaJPEG HQ (app) | 4.3 | 1.4 | 0.21 | 1.00 | almost no change |
| FBCNN gray | 2.2 | 0.2 | 0.16 | 1.00 | no change |
| IllustrationJaNai V3denoise 2x | 5.0 | 1.1 | 0.19 | 0.99 | almost no change on these pages |
| IllustrationJaNai V3denoise 4x | 18.7 | 1.1 | 0.18 | 1.04 | almost no change |
| AnimeSharpV4 2x | 17.3 | 2.1 | 0.20 | 1.06 | faithful, slight sharpening |
| IllustrationJaNai DeJPEG then AnimeSharpV4 | 30.7 | 2.5 | 0.33 | 1.11 | faithful |
| 1xDeJPG RealPLKSR | 8.1 | 9.1 | 0.21 | 1.55 | sharper, lifts highlights |
| FBCNN colour | 2.1 | 0.4 | 0.18 | 1.00 | no change |
| UltraSharpV2 4x | 103.4 | 10.3 | 0.21 | 1.68 | invents outlines on colour art; drop |

**Bug in the shipped app:** the MangaJaNai presets turn colour pages grey (mean saturation 66 to 0.6 on the
colour cover and spread). The MangaJaNai V1 models are grey-only RGB networks and `denoise.py` sends a
colour page's RGB to them. MangaJaNaiConverterGui sends colour pages to IllustrationJaNai instead.

## Round 2 (cleaned pages, 2026-11-14)

The user picked Real-CUGAN 2x conservative and MoESR v2 descreen for B&W from round 1, and asked for one
colour model, picked on the joined spread. 146 runs, 167 s on Modal, each at page size (1x) and at twice
the page (2x, 1672x2400, the done pages' size).

Colour, on the cleaned 50/002+003 spread at 2x (these pages carry more JPEG noise than the done pages, so the
models differ more here than in round 1):

- **IllustrationJaNai V3denoise 4x (FDAT): chosen.** Crisp lines, keeps strawberry seeds and eye highlights,
  colours unchanged. This is also MangaJaNaiConverterGui's default for colour pages.
- Real-CUGAN 2x: clean but flattens fine detail (strawberry texture, iris).
- IllustrationJaNai V3denoise 2x and AnimeSharpV4 2x: good, slightly softer than the 4x.
- IllustrationJaNai DeJPEG then AnimeSharpV4: hatching artefacts in hair.
- 1xDeJPG RealPLKSR: no 2x output; not compared at 2x.

MoESR v2 turns colour grey (it is a B&W descreen model): 49/001, grey with a small pink title logo, came out
with no colour. The delivered page keeps MoESR's luma and puts the source's colour back (YCbCr merge). The app
needs the same for mostly-grey pages, or must send them to the colour model.

Delivered next to the cleaned chapters, nothing overwritten:
`第49話_denoised/` and `第50話_denoised/`, each with `cugan-1x`, `cugan-2x`, `moesr-v2-1x`, `moesr-v2-2x`.
Chapter 50 pages 001 to 003 are the colour model's in every set; 002 and 003 are the two halves of the
denoised spread (left half is 003).

Run 3 (same day): cleaned chapters 51 to 55, 66 pages, all B&W, the same four sets, 264 runs in 276 s.
Five pages carry the small pink title logo (51/002, 52/001, 53/001, 55/001); their MoESR pages got the same
colour merge. Written to `第51話_denoised/` to `第55話_denoised/`.

Copies of the three umzi models, the round 1 compare page and the round 2 colour outputs are in
`~/dev/manga-bench-sets/denoise-2026-11/`. Round 2 cost under $0.50.

## Plan for the app (done 2026-11-14)

All six steps are done on branch `claude/denoise-app-3197e1`. The worker is not redeployed: a live profile
gets the new presets only after its worker is redeployed.

1. **Presets. Done.** Four presets: `realcugan-2x-auto`, `moesr-descreen-auto`, `ijanai-v3-denoise-4x` and
   `waifu2x-scan-4x-n2`. Each offers page size or 2x. The new files are pinned by SHA-256 in `denoise.py`
   and mirrored in `denoise-presets.json`. MangaJaNai is no longer a preset. It stays as a model, and it now
   runs on the luma only with the source colour put back, so it cannot grey a colour page. The old preset
   ids still have names, so the history of an old chapter reads.
2. **Automatic colour routing. Done.** A recipe can carry `color_steps`. A page is colour when at least 8%
   of all its pixels have a chroma (max minus min) of 24/255 or more. The grey page with a small logo
   (49/001) stays on the B&W route and keeps its logo colour. A worker without the `color_route` feature
   is refused before upload.
3. **Advanced chain. Done.** Under Advanced in the denoise dialog, cloud only: up to 3 steps from a list of
   9, and the output scale those steps can give. The run is recorded as preset `custom`.
4. **Keep earlier runs. Done.** No run cap. A new run goes into its own `run-N` folder when the folder
   already holds a run. The compare dialog marks the run in use, sets another one in use, and deletes a
   run after an inline question. Each run shows its disk use.
5. **Preview screen. Done.** One page, wipe, run picker, Fit / 1:1 / 2:1 of the denoised file, page
   stepping. Full window and the folder line are gone.
6. **Install size. Done.** The worker image pins spandrel 0.4.2 and spandrel_extra_arches 0.2.0.
   `install_extra_arches()` runs once per process and tolerates a second registration.

Open points:

- MoESR's host refuses Modal (HTTP 403). The file is optional on the volume: upload it per profile with
  `spikes/denoise/put-moesr.sh <profile> <volume> <pth>`. Until then the worker does not list the preset.
- MoESR states no licence. IllustrationJaNai is CC BY-NC-SA 4.0. The app is non-commercial.
- Smoke test on a real GPU (yarafiq1971, scratch app `mc-denoise-probe-1114`, deleted): colour pages ran
  IllustrationJaNai, grey and logo pages ran the B&W model, the logo kept its colour (saturation 119.6
  against 120), and 1x and 2x sizes were right.

## How to rerun

Harness: `spikes/denoise/combo-bench/` (`bench_app.py`, `combos.py`, `sheet.py`). Pages and outputs live on a
Modal volume, and a Modal function drives the runs, because streaming pages from the laptop dropped the link.

```bash
MODAL_PROFILE=yarafiq1971 modal volume put mc-denoise-bench-2611 <pages dir> /in
MC_DENOISE_PY=deploy/cloud/common/denoise.py MODAL_PROFILE=yarafiq1971 modal run --detach spikes/denoise/combo-bench/bench_app.py::main --crops <boxes.json>
MODAL_PROFILE=yarafiq1971 modal volume get mc-denoise-bench-2611 /crops <dir>
```

Cost: round 1 about $2.97 on yarafiq1971, including failed starts. L4 had no capacity that day; the class
falls back to A10G and L40S.
