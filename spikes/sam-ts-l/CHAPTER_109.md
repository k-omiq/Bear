# Apocalypse 109: full-chapter mask and detector probe

The local input was `/Users/caved/Downloads/apocalypse/109`: 45 JPEG pages,
numbered 01–45, each 690 × 1600. This is a Korean color webtoon, whereas
the published SAM-TS-L and COO evaluations use Japanese manga/comics. The
images and derived masks stay in ignored local `artifacts/`.

## SAM-TS-L mask run

The full pinned Koharu checkpoint (SHA-256
`bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45`)
was strictly loaded into the pinned Hi-SAM architecture (728 keys) once. The
reference preparation and restoration were applied to each page. PyTorch MPS
was chosen from the locally measured page-07 backends because its warmed
inference was faster than ONNX Runtime WebGPU, PyTorch CPU, and ONNX Runtime
CPU on this Apple M5. MPS CPU fallback was disabled. This run returns only
the text-pixel mask; it does not output boxes, instances, OCR, or inpainting.

```sh
spikes/sam-ts-l/artifacts/venv/bin/python spikes/sam-ts-l/run_chapter_torch.py \
  --source-dir /Users/caved/Downloads/apocalypse/109 \
  --output-dir spikes/sam-ts-l/artifacts/examples/chapter-109-torch-mps \
  --device mps
```

All 45 pages completed in **137.217 s** including the one model load and mask
writes. Synchronized forward plus logits transfer: mean **3.009 s**, median
**3.003 s**, p95 **3.585 s** per page. Full per-page work: mean **3.022 s**,
median **3.013 s**, p95 **3.599 s**. Model CPU load took 967 ms and MPS
transfer 232 ms. Peak process RSS was **2,936,553,472 bytes (2.94 GB)**.
Maximum sampled Metal driver allocation was **5,546,885,120 bytes (5.55 GB)**;
the sampler polled every 20 ms and may miss a transient peak. These are
overlapping views of Apple unified memory and must not be added. This was one
run, so the times do not establish a performance distribution across launches.

Each output is a binary 690 × 1600 PNG. The 45 masks contain 1,124,691
foreground pixels in total; page 30 is empty and its source page appears to
have no text. Pages 07–09 matched the saved FP32 PyTorch reference masks with
**zero changed restored pixels** each. This checks execution against the
reference, not the reference's semantic accuracy. The local
`artifacts/examples/chapter-109-torch-mps/` directory contains the numbered
masks, page JSON, `summary.json`, a labeled contact sheet, and `masks.zip`.
`package.json` records the ZIP SHA-256
`8ebaea881f1665461cf50de2a240f83cb11bf80b6c899717ae6cfcca98d03397`.
Compact run results are in `results/chapter-109-torch-mps.json`.

## RT-DETR-v2 versus SAM-TS-L

An isolated Rust probe used the app's actual JPEG decoder and
`BalloonDetector::detect` with its existing 640 × 640 preprocessing and
0.35 score threshold. It opened one CPU ONNX Runtime session for all pages.
The local int8 checkpoint SHA-256 was
`5fe9e4f576e49d4e7e8b0e029d6d3cdc252abd4694113e1cae120e62c931ea79`.

```sh
cargo build -p chapter-rtdetr
ORT_DYLIB_PATH=/Users/caved/dev/manga-cleaner/runtimes/onnxruntime-osx-arm64-1.28.0/lib/libonnxruntime.dylib \
  target/debug/chapter-rtdetr /Users/caved/Downloads/apocalypse/109 \
  /Users/caved/dev/manga-cleaner/models/comic-text-and-bubble-detector-detector-v4-s_int8.onnx \
  spikes/sam-ts-l/artifacts/chapter-109-rtdetr.json
spikes/sam-ts-l/artifacts/ort-webgpu-venv/bin/python spikes/chapter-rtdetr/agreement.py \
  --boxes spikes/sam-ts-l/artifacts/chapter-109-rtdetr.json \
  --masks spikes/sam-ts-l/artifacts/examples/chapter-109-torch-mps/masks \
  --output spikes/sam-ts-l/results/chapter-109-rtdetr-sam-agreement.json
```

RT-DETR produced 77 boxes: 33 `bubble`, 30 `text_bubble`, and 14
`text_free`. It produced no box on 24 pages and no text-class box on 26 pages.
SAM produced a nonempty mask on 25 of those 26 pages. Only **216,644 / 1,124,691
(19.26%)** SAM pixels fell inside the union of RT-DETR text-class boxes;
43/44 text-class boxes intersected some SAM pixels. Bubble boxes are containers
and should not be counted as text detections. Text-box agreement was unchanged
by 2 or 5 pixels of box padding. RT-DETR session open took 84 ms; detector
execution totaled 4.15 s, median 90.6 ms/page; JPEG decode totaled 4.91 s.
These are **model agreement and timing**, not precision, recall, or IoU against
human labels.

There is direct visual evidence of some RT-DETR misses: page 02 has two large
Korean sound effects (approximately x280–430/y140–340 and x100–400/y900–1165)
and page 07 has large effects; both pages have corresponding SAM pixels while
RT-DETR returned zero boxes. A stratified visual inspection of pages 01, 07,
12, 18, 23, 30, 38, and 45 found that prominent text generally follows the
lettering; the small centered chapter label on page 01 is captured but its
thin strokes are fragmented. Page 35 has 609 positive SAM pixels, mostly a
fragment at the bottom-right crop edge over a character's clothing that
appears to be a drawing false positive. It warrants a human decision before
cleanup. This inspection is not a full annotation or an accuracy estimate.
The raw RT-DETR boxes are in ignored
`artifacts/chapter-109-rtdetr.json`; per-page agreement is in the compact JSON
under `results/`.

## Default recommendation and accuracy gate

For a **quality-oriented text-mask pass on this chapter**, use full-page
SAM-TS-L as the primary lettering mask. Run RT-DETR in parallel when box,
bubble, or region context is needed; its boxes must **not gate** SAM inference
or remove SAM pixels. This preserves the observed effects that RT-DETR did
not box while retaining RT-DETR's distinct structural output. A SAM-only
mask preview is a useful simpler option if the user needs only text pixels.
Neither path supplies text instances, reading order, OCR, or a cleaning
decision by itself. This is a provisional workflow recommendation, not a
claim that a particular combination has the best measured accuracy.

The MPS choice is specific to this Mac. A desktop default should choose among
the user's available, qualified CPU/GPU backends on each operating system;
CUDA, DirectML, MIGraphX, and Windows/Linux WebGPU performance and mask parity
have not been measured by this chapter run. Avoid automatic cleanup of every
positive SAM pixel until a review/false-positive policy is established.

This report predates the separate COO MTSv3 chapter run. The three-model
comparison is now recorded in [the combo report](../chapter-combo/REPORT.md).
COO's published task concerns Japanese onomatopoeia, and its checkpoint
redistribution/commercial-use rights remain unresolved. The comparison is
still not a ground-truth accuracy ranking for this Korean webtoon.
The [SAM-TS-L model card](https://huggingface.co/mayocream/koharu-text-sam-ts-l)
describes text-pixel segmentation and limitations on small/stylized lettering;
the [RT-DETR model card](https://huggingface.co/ogkalu/comic-text-and-bubble-detector)
describes its bubble/text-box classes; the
[COO model card](https://huggingface.co/mayocream/coo-comic-onomatopoeia-safetensors)
describes its Japanese SFX task. Those published evaluations are not a
ground-truth score for this chapter.

To choose an accuracy winner, annotate a stratified sample of these pages
with text pixels, individual lettering regions, and bubble/SFX classes.
Score SAM alone, RT-DETR alone, the independent union, and COO additions
against the **same** labels.
Measure mask precision/recall/IoU and region detection recall/precision,
stratified by dialogue, SFX, tiny text, and art-like marks. Inspect cleanup
effects before any default is promoted to the desktop app. No UI, catalogue,
runtime routing, cloud deployment, or M0–M1 code was changed by this probe.
