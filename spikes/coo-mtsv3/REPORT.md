# COO MTSv3 on Apocalypse chapter 109

The local MPS probe processed all 45 user-provided pages using the pinned
`best_test.yaml` detection evaluation preprocessing. Its
[evaluation transform](https://github.com/ku21fan/COO-Comic-Onomatopoeia/blob/d8028f015b8ce99a4dd798427342f97087529357/MTSv3/maskrcnn_benchmark/data/transforms/build.py#L62-L70)
uses PIL resize before tensor conversion and BGR255 normalization. It uses the published
1440-pixel minimum side and 4000-pixel maximum side. Pages at 690×1600 were
resized to 1440×3339, padded to 1440×3360, converted to BGR255, and
normalized by Caffe means `[102.9801, 115.9465, 122.7717]`. All 300
checkpoint tensors loaded strictly after full SHA-256 verification.

| Measurement | Result |
| --- | ---: |
| Pages | 45 |
| Polygon proposals, box score ≥0.1 | 203 |
| Polygon proposals, demo filter ≥0.4 | 58 on 31 pages |
| Pages with zero proposals at 0.1 | 11 |
| Mean/median prepare | 64.26 / 66.45 ms |
| Mean/median neural inference | 1314.22 / 1344.20 ms |
| Mean/median contour postprocess | 3.92 / 3.54 ms |
| Inference across chapter | 59.14 s |
| Peak process RSS | 934,395,904 bytes |

Timings exclude checkpoint load and artifact writes. Peak RSS is process
memory; it is not a separate measure of Apple unified GPU allocations.
The preserved run is in `artifacts/chapter-109-eval-pil/`; the old
`artifacts/chapter-109/` path remains a symlink to it. `summary.json`
includes each page's image hash, prepared tensor hash, raw
proposal map hash, dimensions, detections, and stage timings. `manifest.json`
includes SHA-256 and byte size for every artifact.
The final output directory contains 144 files totaling 862,806,323 bytes.
`summary.json` is 881,893 bytes with SHA-256
`d15f266718c8fb3f9a91bd8c857ccdd96b6b194c7bb977ab5095ae0246f38aa1`;
`manifest.json` is 21,057 bytes with SHA-256
`f4ec270fd6395c44256809ae43a517ba0bc53be5d891545f5e858cbec21d565e`.

Visual examples:

- Page 02: Two proposals survive the 0.4 demo filter. The upper isolated
  Korean sound effect is enclosed at score 0.608; a much larger lower sound
  effect is enclosed at 0.449, with several nested low-score fragments.
- Page 07: One lower sound-effect fragment survives at 0.476. The page has
  other prominent lettering the detector did not enclose as complete
  polygons. Several smaller fragments score 0.11–0.34.
- Page 24: A top sound-effect fragment scores 0.471. A bottom rock silhouette
  scores 0.415 and is a visible false candidate; its SAM lettering overlap
  was reported as zero in the combined review. This demonstrates why a COO
  polygon cannot be used directly as an erase mask.
- Page 35: The raw proposal map has no contour above 0.1 and yields no
  polygon proposal.

The chapter overlays are in ignored
`artifacts/chapter-109-eval-pil/overlay-{02,07,24,35}.png` and
`artifacts/chapter-109-eval-pil/contact-sheet.png`. Numeric counts are model output,
not precision or recall; the pages have no new hand-labeled truth set. The
incremental value of COO must be judged against the full RT-DETRv2 + SAM-TS
run using the same source coordinates, then weighed against these false
candidates, roughly 1.3 s/page inference, memory, and non-commercial terms.

## Preprocessing source check

The pinned [standalone demo](https://github.com/ku21fan/COO-Comic-Onomatopoeia/blob/d8028f015b8ce99a4dd798427342f97087529357/MTSv3/demo.py#L53-L80)
also resizes through PIL before normalization, but its `main` uses an
[800-pixel minimum side](https://github.com/ku21fan/COO-Comic-Onomatopoeia/blob/d8028f015b8ce99a4dd798427342f97087529357/MTSv3/demo.py#L296-L298).
The pinned [M4C feature extraction path](https://github.com/ku21fan/COO-Comic-Onomatopoeia/blob/d8028f015b8ce99a4dd798427342f97087529357/MTSv3/extract_features_vmb.py#L195-L225)
instead subtracts the Caffe means in float32 BGR before OpenCV linear resize.
The evaluation path is appropriate for the reported `best_test.yaml` 45-page
detection run. The alternate feature path was probed separately on pages 02
and 07 as a sensitivity check; it did not replace the official evaluation
result.

The saved evaluation inputs on pages 02/07 are byte-identical to a fresh
`eval_pil` preparation. Against `feature_cv2` at the same padded shape,
their mean absolute tensor differences were 0.426/0.330 and maximum
differences 15.01/14.19. The resulting proposal-map mean absolute
differences were 0.000255/0.000135; 1,283/668 pixels changed sides of the
0.1 contour threshold. Both runs produced 11/5 polygons at score ≥0.1 and
2/1 at score ≥0.4 on pages 02/07. The two-page alternative artifacts and
per-proposal scores are in `artifacts/chapter-109-feature-cv2-smoke/`, and
prepared tensors are in `artifacts/preprocessing/`. This sample shows the
ordering changes numbers but not the proposal counts on those pages; it is
not a chapter-wide equivalence result.
