# Live three-model detection pipeline: Apocalypse 109

The requested **combined pipeline** ran on all 45 JPEG pages at
`/Users/caved/Downloads/apocalypse/109`. This is a single command that executes
the full ogkalu RT-DETR-v2 FP32 graph on two vertical tiles, the full pinned
Koharu SAM-TS-L mask model, and COO MTSv3 detection-only proposals, then joins
their source-coordinate evidence into one candidate list per page. It is a
local Python proof. The intended desktop implementation remains Rust/ONNX
Runtime, and this run changes no app UI, catalogue, routing, cloud service, or
M0–M1 checkout.

## What the fusion actually does

- RT text boxes supply `text_free` or `text_bubble` region evidence. RT
  `bubble` boxes are kept as context, never counted as lettering evidence.
- COO polygons supply optional SFX grouping evidence. Their full polygon
  areas are never treated as erase masks.
- Every 8-connected SAM mask component becomes a candidate with exact pixel
  geometry in a 16-bit label PNG. All components survive even when neither
  detector overlaps them. Every intersecting region is linked many-to-many
  with shared-pixel counts and overlap fractions. A detector region without
  SAM pixels becomes a reviewable detector-only candidate with no mask.
- When one COO polygon touches several SAM components, the output suggests a
  possible fragment group. It does not merge pixels or declare a text
  instance. No pixels are added to or removed from the SAM mask by fusion.

This is late fusion of the three real model outputs, not a jointly trained
model. Running model kernels simultaneously cannot itself improve their
predictions. Here RT CPU and SAM Apple MPS ran concurrently; COO ran on MPS
after SAM released it, to avoid contention between the two large GPU models.
Fusion ran after all three outputs were available.

## Live result

| Measurement | 45-page result |
| --- | ---: |
| One-command wall time, including load, image preparation, saved artifacts and fusion | **224.945 s** |
| RT wall time, concurrent with SAM | 44.915 s |
| SAM wall time, concurrent with RT | 154.116 s |
| COO wall time, following SAM | 69.841 s |
| Fusion wall time | 0.981 s |
| Unified review candidates | **926**: 922 SAM components plus 4 detector-only regions |
| RT boxes | 71 text-class plus 39 bubble-context boxes |
| COO polygons at score ≥0.4 | 58 |
| Geometric component/region links at ≥1 shared pixel | 1,408 |
| Possible COO fragment-group suggestions | 51 on 30 pages |
| SAM components with no RT text or COO overlap | 209 across 30 pages |

These are raw fragments, proposals, and geometric links, **not 926 proven
lettering instances**. A single SFX may have multiple SAM components, a broad
region can link unrelated fragments, and an incidental one-pixel overlap can
make a link. The JSON records each link's component and region fractions so
review or future thresholds need not assume every link is strong.

The concurrent run reproduced all prior independent outputs: **45/45** RT
page box lists identical, **45/45** COO detection lists identical, and
**45/45** SAM mask PNG hashes identical. The fused candidate list improved
association and review information, while the lettering-pixel mask remained
exactly the same. The earlier geometric comparison found 61.20% of SAM
pixels inside an RT text box or COO polygon. This is candidate coverage
relative to SAM, not accuracy against human labels; see
[`REPORT.md`](REPORT.md).

The only four detector-only text/SFX candidates were visually audited:
the COO proposal on page 24 encloses rocks; RT's page 26 box follows a blank
speech-balloon edge; its page 39 and 44 boxes enclose debris. All four look
like art, with no visible lettering. This small complete audit of the
detector-only set found **no visible extra text for SAM to recover** from those
regions on this chapter. It does not establish chapter-wide mask precision
or recall. See the [candidate audit](results/live-chapter-109-detector-only-audit.json).

The grouping and retention behavior matters on specific pages:

- Page 02: RT found no box. COO's two polygons suggest two groups from seven
  SAM components, corresponding to the two visible sound effects.
- Page 16: two SAM components remain reviewable despite no RT text box or COO
  polygon. The large clipped glyph at the bottom is visibly lettering.
- Page 24: the COO rock proposal has no SAM pixel and stays a detector-only
  review candidate. It cannot become an erase region automatically.
- Page 30: all three models are empty. Page 35's four SAM-only components are
  visually suspect art fragments, illustrating why unassociated does not
  mean true lettering.

## Time, memory and storage

The largest 100 ms sampled **sum of active child process RSS** was 2.212 GB
during concurrent RT+SAM. Per-process high-water readings from the model
processes were 2.936 GB for SAM and 0.922 GB for COO; `ps` sampling missed
some of SAM's shorter memory peak, so the 2.212 GB sampled sum is **not a
maximum bound**. Maximum sampled PyTorch Metal driver allocation was
5.549 GB for SAM and 3.805 GB for COO. Those GPU measurements occur in
different phases and overlap macOS unified-memory RSS; they must not be
summed. No paid cloud provider was used.

The COO output directory used 852 MB, mostly dense diagnostic proposal maps.
That is proof artifact storage, not a required packaged checkpoint size. The
COO checkpoint is about 106 MB; the RT ONNX graph is 168.48 MB, and the SAM
checkpoint is 1.356 GB. Each model's per-page timing and provider details
remain in its own saved result.

## Reproduce and inspect

```sh
python3 spikes/chapter-combo/run_pipeline.py \
  --source-dir /Users/caved/Downloads/apocalypse/109 \
  --output-dir spikes/chapter-combo/.cache/live-chapter-109
python3 spikes/chapter-combo/render_fused.py \
  spikes/chapter-combo/.cache/live-chapter-109
```

`run_pipeline.py` requires a fresh output directory. A two-page check used
`--pages 02 07` and reproduced the saved model outputs exactly before the
full run. The complete run's [stage/command/memory record](results/live-chapter-109-run.json)
and [fusion summary](results/live-chapter-109-fusion-summary.json) are tracked.
Per-page candidate JSON, exact component-label PNGs, model outputs, logs,
source hashes, and the eight rendered visual previews are in ignored
`spikes/chapter-combo/.cache/live-chapter-109/`. The renderer verifies source,
mask and label checksums and dimensions before drawing. The
[fusion code](fuse.py) and [runner](run_pipeline.py) are small, reproducible
local probes; no manga-ocr, cloud job or inpainting model is involved.

## Decision from this chapter

For a mask-only task, SAM supplies the pixels and the other models do not
change this chapter's mask. RT adds bubble/text region context. COO adds
useful SFX grouping suggestions, notably when RT misses entire effects, and
belongs in an optional discovery mode. Keep SAM-only components reviewable;
do not gate the mask on RT/COO or auto-erase detector-only proposals. To
establish an accuracy improvement, annotate true text pixels and instances
on a stratified page sample and evaluate pixel and region errors against
those labels. A proposal-guided second SAM crop pass would require that
evaluation before adding pixels; the four detector-only regions in this run
are all apparent art false positives.

COO MTSv3's component code carries non-commercial terms and the converted
weight card does not grant clear checkpoint redistribution/commercial rights.
This local run does not clear bundling or service use.
