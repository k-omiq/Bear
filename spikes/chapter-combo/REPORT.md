# Chapter 109: RT-DETR-v2 + COO MTSv3 + SAM-TS-L

This report records the initial separate-model comparison. The later
[one-command live fused run](FUSED_PIPELINE.md) executed all three models and
produced combined per-page candidates, measured wall time and memory.

This is the local, same-page combination probe requested for the 45 JPEGs in
`/Users/caved/Downloads/apocalypse/109` (690 × 1600 each). The source is a
Korean color webtoon. All three models ran; the large outputs and comics stay
in ignored local `artifacts/`. Candidate regions are not lettering erase
masks, and their overlap with the SAM mask is not labeled accuracy.

## What ran

| Role | Exact artifact | Path used here | Result |
| --- | --- | --- | --- |
| Text/bubble boxes | [ogkalu RT-DETR-v2](https://huggingface.co/ogkalu/comic-text-and-bubble-detector), revision `16e8a622f91fabc6b5b65c96d32d1183f8843546` | Full `detector.onnx`, SHA-256 `065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195`, CPU, two vertical halves | 71 text boxes, 110 boxes total; 43.34 s model inference across 45 pages |
| SFX proposals | [COO MTSv3](https://github.com/ku21fan/COO-Comic-Onomatopoeia) detection branch, upstream commit `d8028f015b8ce99a4dd798427342f97087529357` | [Converted checkpoint](https://huggingface.co/mayocream/coo-comic-onomatopoeia-safetensors) revision `b5d31460573b6f61c1d4bdaea5fe4e18425e6a61`, SHA-256 `49a448ec19e4e3894b726553fe6f47916827b10d642109aa5f41b96aeeb88e30`, PyTorch MPS | 203 raw proposals at upstream score ≥0.1; 58 at demo score ≥0.4; 59.14 s neural inference |
| Lettering pixels | Koharu SAM-TS-L full checkpoint, SHA-256 `bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45` | One-load FP32 PyTorch MPS run | 45 binary masks; 135.40 s synchronized inference, 137.22 s whole run |

RT-DETR uses the published 640 × 640 bilinear RGB /255 preprocessing and a
0.35 score threshold. `halves` is a test of two contiguous 690 × 800 vertical
tiles, not a claim to reproduce unknown training split dimensions. The
existing 11 MB small INT8 model and whole-page mode were separately measured
as controls; see [RT variant report](../chapter-rtdetr/FULL_MODEL_COMPARISON.md).
COO uses the pinned `best_test.yaml` evaluation transform: PIL resize at the
published 1440 minimum/4000 maximum bounds, then BGR and Caffe means,
and emits a sigmoid proposal map followed by polygon postprocessing. Its 300
checkpoint keys loaded strictly. The [COO report](../coo-mtsv3/REPORT.md)
records preparation, a source-path check against the separate M4C feature
extraction transform, proposal maps, full manifest, and stage timings. SAM's
page 07–09 masks matched saved FP32 CPU references pixel for pixel; see
[chapter mask report](../sam-ts-l/CHAPTER_109.md).

The initial runs below used separate processes. Their stage times must not be described
as one measured end-to-end combo wall time, and their memory peaks must not
be added. COO's mean neural inference was 1.31 s/page and peak process RSS
was 0.93 GB; Apple GPU allocations were not separately sampled. Full RT
halves ran on CPU with 1.01 s/page median model inference, excluding JPEG
decode/preparation. SAM MPS measured 3.00 s/page median synchronized inference,
2.94 GB peak process RSS, and 5.55 GB maximum sampled Metal driver allocation
(overlapping unified-memory views). The COO checkpoint is about 106 MB, the
full RT graph 168.48 MB, and the SAM checkpoint 1.356 GB; their simultaneous
desktop residency has not been measured.

## Combined candidate coverage

The table counts how many **SAM-positive pixels fall inside** RT text boxes
or COO polygons in original page coordinates. SAM is another model, not
human ground truth. A larger percentage is candidate coverage, not accuracy.
Bubble-only RT boxes are excluded. COO at score ≥0.4 is the upstream demo
filter; the upstream raw box threshold is ≥0.1.

| RT checkpoint / page layout | RT text boxes | SAM pixels in RT text boxes | Additional SAM pixels in COO polygons outside RT | SAM pixels in either region |
| --- | ---: | ---: | ---: | ---: |
| Small INT8 / whole | 40 | 216,644 (19.26%) | 405,403 | 622,047 (55.31%) |
| Small INT8 / halves | 46 | 216,458 (19.25%) | 405,403 | 621,861 (55.29%) |
| Full FP32 / whole | 49 | 222,087 (19.75%) | 399,960 | 622,047 (55.31%) |
| Full FP32 / halves | 71 | 360,917 (32.09%) | 327,369 | 688,286 (61.20%) |

For full FP32 halves, COO had 58 proposals ≥0.4 on 31 pages; 57 polygons
intersected at least one SAM pixel and its polygons added SAM coverage outside
RT boxes on 30 pages. Lowering the COO score to ≥0.1 produced 203 proposals
and raised combined SAM-pixel coverage from 688,286 to 723,181 (64.30%), at
the cost of 145 more proposals. These quantities are in
`results/{fp32-halves,fp32-whole,v4s-int8-whole,v4s-int8-halves}-coo04.json`
and `results/fp32-halves-coo01.json`. [`analyze.py`](analyze.py) verifies
same-source hashes between SAM and COO, rasterizes COO polygons with Pillow,
and saves per-page proposal/box geometry and counts.

## Visible examples and interpretation

- **COO adds candidates:** page 02 has two obvious large Korean sound effects.
  RT-DETR returned no box in all tested checkpoint/layout conditions. COO
  encloses both at scores 0.608 and 0.449; SAM supplies their lettering pixels.
- **SAM remains needed for the text-shaped mask:** page 16 has a large clipped
  stylized glyph at the bottom. SAM marks its strokes, while full tiled RT and
  COO ≥0.4 produce no text candidate. Page 24 also has large effects that are
  incompletely proposed by COO and unboxed by RT. Candidate-gating the SAM
  mask would drop visible lettering.
- **Proposals can mark art:** COO scores a rock silhouette near the bottom of
  page 24 at 0.415; it overlaps no SAM pixels. SAM itself has apparent art
  false positives on some pages, including page 35. Neither model's positive
  output is automatic permission to erase.

The reproducible [example renderer](render_examples.py) writes aligned
four-panel previews for pages 02, 07, 16, and 24 to ignored
`spikes/chapter-combo/.cache/visual-examples/`. Its panels show the original,
full FP32 tiled RT boxes, COO polygons at score ≥0.4, and SAM mask tint.
It verifies source hashes and dimensions before drawing.

The [visual proposal audit](results/coo-proposal-visual-audit.json) reviewed
all 58 COO proposals at score ≥0.4: 54 visibly contain SFX or text, one is
clear art, and three are ambiguous. Eleven of the 54 clear proposals have
at least half their bounding-box area covered by an RT text box. The other
43 are potentially useful additions; this is a count of proposals, not
deduplicated text instances or a measured recall gain. The
[RT box audit](../chapter-rtdetr/results/full-halves-novel-box-audit.json)
separately found four clear art boxes among 29 text-class boxes geometrically
unmatched by the small whole-page model. These visual labels are **human
impressions**, not a fully labeled instance/pixel truth set. COO is trained
on Japanese manga SFX; transfer to this Korean webtoon is empirical and
limited to these pages. Its recognition and truncated-text linking models
were not run or required for this detection comparison.

## Decision for this chapter

For a pure all-text **pixel-mask preview**, full-page SAM is sufficient to
produce the mask and neither detector is a prerequisite. For a workflow that
needs bubble/context boxes and explicit SFX proposal groups, use RT-DETR and
COO alongside SAM, keep their output roles independent, and review unassociated
SAM fragments. COO adds clear SFX candidates on page 02 and many geometrically
new candidates elsewhere, so it is useful as an **optional discovery aid**;
its art false positive and added latency do not justify making it mandatory
or using its polygons as an erase mask. A text-shaped erase workflow still
needs SAM or another validated pixel segmenter; RT+COO alone supply regions.

The full tiled RT model finds more text boxes and covers more SAM pixels than
the small whole-page model, at substantially higher CPU cost. This is a
quality-mode candidate, not a proven accuracy winner. The best general
default among the variants remains unproven until the chapter's SFX, dialogue,
and art regions are labeled and each variant's true positives, misses, false
positives, and safe-cleanup masks are scored against that common truth set.
No app UI, catalogue, routing, cloud deployment, or M0–M1 code was changed.

COO's upstream MTSv3 code references CC BY-NC 4.0, and the converted
checkpoint card says the weight-file license is not explicit. Public download
does not clear redistribution or commercial use. This local comparison does
not promote COO into the desktop default or bundle its checkpoint.

## Reproduce the combination calculation

The individual runs and exact checkpoint commands are documented in the
[RT report](../chapter-rtdetr/FULL_MODEL_COMPARISON.md),
[COO README](../coo-mtsv3/README.md), and
[SAM report](../sam-ts-l/CHAPTER_109.md). With their saved page results:

```sh
python3 spikes/chapter-combo/analyze.py \
  --rt-json spikes/chapter-rtdetr/artifacts/chapter-109-fp32-halves.json \
  --coo-dir spikes/coo-mtsv3/artifacts/chapter-109-eval-pil \
  --sam-dir spikes/sam-ts-l/artifacts/examples/chapter-109-torch-mps/masks \
  --coo-min-score 0.4 \
  --output spikes/chapter-combo/results/fp32-halves-coo04.json
python3 spikes/chapter-combo/render_examples.py
```

The analyzer verifies page hashes, model/checkpoint identities, and saved
COO artifact hashes before measuring source-coordinate region overlap.
