# Model workflow decisions and measured benchmarks

This document records the model workflow decisions behind the optional local text-shaped review, with the benchmarks that
support them: the SAM-TS-L mask-only export, the RT-DETR variants, the combined chapter run, and the resource and
quality limits of the supervised write. The [SAM-TS-L export report](../spikes/sam-ts-l/REPORT.md) holds the export
and FP32 reference record. All timings below are local measurements on a MacBook Air Mac17,3 with an Apple M5 and 32 GB
unified memory, taken in September 2026. They are not Windows/Linux benchmarks or human-label accuracy scores. The
export and chapter spike reports are historical snapshots: their statements that the app had no SAM route were true
when they were measured and are superseded by the desktop work described here.

## Desktop workflow summary

| Area | Current implementation or proof |
| --- | --- |
| Independent desktop analysis | **Regions only** runs RT-DETR-v2, **Mask only** runs SAM-TS-L, and **Text-shaped review** runs RT then SAM on the same page and combines their evidence. The RT choice is the pinned full graph on two vertical tiles or the existing small whole-page graph. Local graph import verifies size and SHA-256. These are review modes; legacy CTD-based Auto Clean remains the default. |
| Source-coordinate fusion | Every 8-connected SAM component survives, including components with no RT box. RT text boxes provide region context; bubble-only boxes are context, and detector-only proposals have no erase pixels. The local three-model research runner also accepted COO SFX polygons as grouping suggestions, without changing the SAM mask. See the [live fusion run](../spikes/chapter-combo/FUSED_PIPELINE.md). |
| Provider paths | Native Rust/ONNX Runtime implements SAM CPU and explicit Apple WebGPU review. Both WebGPU graph sessions disable CPU fallback and verify every profiled node's provider. Separate reproducible Python proof selectors expose PyTorch CPU/MPS/CUDA and ONNX CPU/CoreML/CUDA/DirectML/WebGPU/MIGraphX when installed; they are not a desktop Python dependency or evidence that those devices were qualified. See [provider qualification](provider-qualification.md). |
| Supervised edits | A selected SAM component can become a versioned `MaskPlan` with explicit add/remove corrections and round source-pixel padding. The default is 0 px; the supported engineering range is 0–64 px, not a quality-tuned recommendation. The previewed support raster and its hash are the exact write permission. Prepare/apply bind the model, backend/runtime, source, lower composited underlay, predecessor edits and plan revision. Immutable patch revisions support export and persisted undo/redo. See the [write contract](component-write-contract.md) and [features](features.md). |
| Chapter comparison | All 45 pages of local Apocalypse 109 were run with SAM alone and with RT/COO comparison probes. A one-command three-model research run produced 922 SAM components and four detector-only candidates; the latter four appeared to be art on visual review. Model association improved review information but left all 45 SAM mask hashes unchanged. See the [chapter mask report](../spikes/sam-ts-l/CHAPTER_109.md), [RT variants](../spikes/chapter-rtdetr/FULL_MODEL_COMPARISON.md), and [fusion report](../spikes/chapter-combo/FUSED_PIPELINE.md). |

## Decision changes

1. **Keep independent choices and the existing default.** Users can choose RT
   alone, SAM alone, or RT + SAM for local review on qualified macOS paths.
   The combined mode is late fusion of two outputs, not a jointly trained
   detector or automatic cleanup. The legacy CTD + RT cleaning flow remains
   the default. Manga-ocr is a separate optional legacy reader and is not
   required for the all-text SAM mask. See [workflow presets](../src/lib/model/pipelines.js).
2. **Never gate SAM pixels on detector proposals.** RT adds boxes and bubble
   context; it cannot replace the high-resolution lettering mask. The full
   two-tile RT variant found more boxes than the small whole-page variant on
   this chapter, but box count and overlap with SAM are not accuracy. No
   variant becomes a universal default without a human-labeled holdout.
3. **Exclude COO from the desktop product path.** COO contributed SFX grouping
   suggestions in the local research run, but its [converted checkpoint
   card](https://huggingface.co/mayocream/coo-comic-onomatopoeia-safetensors)
   does not grant clear weight redistribution or commercial rights, and the
   upstream MTSv3 code is noncommercial. It has no desktop importer, runtime
   route, download, bundle, or write authority. This is a scope decision, not
   rights clearance. See [rights decision](model-rights-decision.md).
4. **Restrict write authority to measured evidence.** On the measured M5,
   the app's pinned ORT 1.28.0 WebGPU runtime matched all 45 saved PyTorch MPS
   masks on identical Pillow-prepared tensors. Native CPU matched 44/45; one
   near-zero page-27 logit changed two restored pixels. CPU remains available
   for review, while only explicit WebGPU analysis of a PNG library page can
   prepare a component write. Other Macs, Windows/Linux, JPEGs and longstrips
   remain review-only. See [page-27 diagnosis](../spikes/sam-ts-l/results/native-page27-parity.md)
   and [write contract](component-write-contract.md).
5. **Use the desktop decoder as the desktop input contract.** On page 27, the
   native JPEG decoder differed from Pillow in 104,342 RGB channel samples
   and produced 666 different ONNX mask pixels. Identical prepared tensors
   give identical Python/native ORT CPU masks; this is a decoder decision,
   not proof that original JPEGs match the Pillow reference. A future JPEG
   write qualification must benchmark the app-decoded raster directly.
6. **Hold broader release claims.** CoreML encoder setup did not complete in
   the bounded experiments. Windows/Linux GPUs need matching physical hardware
   and provider/node/memory/parity measurements. Human-labeled mask and
   reconstruction quality, model distribution rights, and broad automatic
   cleaning remain unqualified. No paid cloud job was started. See
   [provider qualification](provider-qualification.md).

The CoreML encoder did not finish session construction within 180 seconds in
the bounded probe. Its head alone split 44 node events to CoreML and 52 to
CPU and changed four restored pixels, so provider availability did not make
it an alternate qualified desktop path. See [provider observations in the export report](../spikes/sam-ts-l/REPORT.md).

## Benchmark ledger

### Controlled page 07: PyTorch versus mask-only ONNX

All four probes used the same saved, Pillow-prepared page-07 FP32 tensor on the
M5, one warmup and two timed inference passes in separate processes. Medians
cover inference and logits transfer; they exclude process startup, model load,
image decode, parity checking and the separate profiling pass. CPU used four
threads. PyTorch ran the full checkpoint; ONNX ran the exported encoder and
text head. ORT CPU used 1.23.2 and the WebGPU probe used 1.28.0, so the table
is an observed path comparison, not a same-runtime engine microbenchmark.

| Path | Warm median | Peak process RSS | macOS peak memory footprint | Restored mask pixels changed vs saved FP32 reference |
| --- | ---: | ---: | ---: | ---: |
| PyTorch CPU | 4.18 s | 5.68 GB | 5.62 GB | 0 |
| PyTorch Apple MPS | 2.64 s | 2.96 GB | 6.78 GB | 0 |
| ONNX Runtime CPU | 11.18 s | 10.87 GB | 10.88 GB | 0 |
| ONNX Runtime WebGPU | 2.84 s | 2.55 GB | 7.13 GB | 0 |

The MPS probe additionally sampled 5.53 GB of Metal driver allocation. The
WebGPU row did not independently sample GPU allocation. RSS, macOS footprint
and Metal allocations are different, overlapping views of unified memory;
do not add them or treat the smallest one as the total memory requirement.
The exact readings and timing samples are in
[`controlled-page07-external-timer.json`](../spikes/sam-ts-l/results/controlled-page07-external-timer.json),
[`torch-cpu-page07.json`](../spikes/sam-ts-l/results/torch-cpu-page07.json),
[`torch-mps-page07.json`](../spikes/sam-ts-l/results/torch-mps-page07.json),
[`ort-cpu-page07.json`](../spikes/sam-ts-l/results/ort-cpu-page07.json), and
[`ort-webgpu-page07.json`](../spikes/sam-ts-l/results/ort-webgpu-page07.json).

Mask-only ONNX is not inherently smaller or faster: the two graphs total
1,358,010,626 bytes, **2,185,638 bytes larger** than the original
SafeTensors checkpoint. The live mask path retains 99.755% of tensor-data
bytes, mainly the ViT-L encoder. CPU ORT's measured 10.87 GB RSS and 11.18 s
warm run are worse than the PyTorch CPU observation above. The speed benefit
on this host came from a suitable GPU provider, not from the ONNX file format
or removal of unrelated heads. See [graph inventory in the export report](../spikes/sam-ts-l/REPORT.md).

### Whole chapter and native desktop provider

The chapter corpus is 45 local 690×1600 JPEG pages from the Korean color
webtoon Apocalypse 109. The scans and large derived arrays remain ignored
local artifacts; the tracked results below contain metrics and identities.

| Run and scope | Time | Memory | Mask comparison |
| --- | --- | --- | --- |
| Full PyTorch MPS, 45 JPEG pages, one checkpoint load | 137.217 s whole run; 3.003 s median synchronized inference/page | 2.937 GB peak RSS; 5.547 GB maximum sampled Metal driver allocation | Pages 07–09 each had 0 changed pixels against their saved FP32 references; all 45 masks were saved for later comparisons. |
| Native Rust/ORT 1.28 CPU, 45 Pillow-decoded pages, sequential sessions | 5.723 s median encoder + head/page; 6.327 s including repeated session load | 8.548 GB process RSS high-water | 44/45 masks exact against saved PyTorch masks; page 27 changed 2 pixels. |
| App-downloaded ORT 1.28 built-in WebGPU, 45 identical Pillow-prepared tensors, one session pair | 6.303 s median encoder + head + raw write/page; 689/37 ms first encoder/head session load | 2.475 GB peak RSS; 5.949 GB sampled Metal high-water | 45/45 saved PyTorch MPS masks exact, 0 changed pixels; direct page profiles had 1,818 encoder and 386 head WebGPU nodes, 0 CPU. |

These rows use different preparation, inclusion of writes, session lifetimes
and host bindings; do not rank them as a single end-to-end desktop speed test.
The WebGPU chapter probe used Pillow-prepared JPEG tensors, whereas the app
defines its own decoded raster as input. Its Metal sampler polled at 20 ms and
can miss transients. The source figures are in [the chapter MPS result](../spikes/sam-ts-l/results/chapter-109-torch-mps.json)
and [the app-runtime WebGPU result](../spikes/sam-ts-l/results/native-rust-app-webgpu-45-pillow.json).

### Detector variants and combined chapter run

The following RT times are `session.run` medians on 45 pages, excluding JPEG
decode and preprocessing. “SAM pixels in text boxes” measures agreement with
another model's output, not recall against human labels.

| RT checkpoint / input | Text boxes | Median inference/page | SAM pixels in RT text boxes |
| --- | ---: | ---: | ---: |
| Existing small INT8, whole page, app nearest control | 44 | 90.6 ms | 19.26% |
| Existing small INT8, whole page, bilinear | 40 | 69.3 ms | 19.26% |
| Full FP32, whole page, bilinear | 49 | 530.6 ms | 19.75% |
| Full FP32, two vertical tiles, bilinear | 71 | 1,007.6 ms | 32.09% |

The full split added 29 geometrically unmatched text boxes relative to small
whole-page; visual review judged 25 to contain text/effects and four to cover
art. That targeted audit does not establish precision or a better default.
The fifth measured variant, small INT8 on two bilinear tiles, produced 46 text
boxes at 144.9 ms/page and 19.25% overlap. See the [RT variant report](../spikes/chapter-rtdetr/FULL_MODEL_COMPARISON.md).

A separate **one-command research fusion run** executed full two-tile RT on
CPU concurrently with SAM on MPS, then COO on MPS after SAM released it.
Its 45-page wall time was **224.945 s** including model loading, image
preparation, outputs and fusion. It produced 926 review candidates (922 SAM
components plus four detector-only proposals), 58 COO polygons at score
≥0.4, and 51 possible COO fragment-group suggestions on 30 pages. The
four detector-only proposals appeared to cover art on visual review. All
45 RT box lists, COO detection lists and SAM mask hashes matched their
separate runs; fusion did not change erase pixels. Its sampled sum of active
child RSS peaked at 2.212 GB, **not a peak bound** because sampling missed
some shorter peaks; the SAM child reached 2.936 GB RSS separately. MPS and
RSS allocations overlap in unified memory. See [live run and resource
details](../spikes/chapter-combo/FUSED_PIPELINE.md) and
[machine-readable run result](../spikes/chapter-combo/results/live-chapter-109-run.json).

As a geometric comparison, full two-tile RT text boxes contained 360,917
SAM-positive pixels (32.09%). COO polygons at score ≥0.4 covered another
327,369 SAM pixels outside those boxes, so either proposal type covered
688,286 pixels (61.20%). COO's neural inference took 59.14 seconds over the
chapter in its earlier separate run. Neither coverage nor proposal count is
human-label recall, and this research value does not override the COO rights
exclusion. See [three-model comparison](../spikes/chapter-combo/REPORT.md).

### Supervised write path: resources and quality limits

These rows measure the write path (page decode, SAM analysis, component preparation, apply, export) on the same M5 host.

| Sample | Observation | Limit |
| --- | --- | --- |
| 4000x6000 Gray8 page + 128x128 prepared mask | 24,000,000 decoded page bytes; 33,701,888 B process RSS high-water; 26,804,704 B reported peak footprint | No ONNX, renderer or PSD in this measurement. |
| 4000x6000 synthetic white L8 SAM-TS-L WebGPU run, app ORT 1.28.0 | 15.879 s total; 2,625,880,064 B process RSS high-water; 5,903,499,264 B Metal high-water sampled at 20 ms | One synthetic page. Metal sampling can miss transients and overlaps RSS in unified memory. |
| Saved 45-page Korean colour webtoon chapter (Apocalypse 109), identical Pillow-prepared input, WebGPU | 45/45 restored masks match saved PyTorch MPS masks; zero changed pixels | Mask parity, not human-label accuracy or reconstruction quality. |
| Native CPU on the Pillow-decoded chapter | 44/45 exact restored masks; page 27 differs by two threshold pixels | CPU stays a review path. |

An integration test with the real WebGPU graphs ran native SAM inference, component preparation, hash-approved apply and lossless export, and every changed decoded pixel lay inside the approved support. That validates the write bridge on the measured host. It does not measure reconstruction quality.

No rights-cleared, untouched Japanese or Korean human-labelled holdout exists, with instance, complete-page, text-pixel, artwork-damage, false-candidate and correction-time measures. Quality claims for automatic text-shaped cleaning therefore stay unmade. Windows and Linux providers have no matching-hardware measurement.

### Known failure cases

| Case | Artifact | Product response |
| --- | --- | --- |
| Near-zero logit changes two CPU mask pixels on page 27 | [stage diagnosis](../spikes/sam-ts-l/results/native-page27-parity.md) | CPU review only; no exact-parity write approval. |
| Original JPEG decoding changes the SAM input, and 666 mask pixels on page 27, relative to Pillow | Decision 5 above | JPEG review only; the native decoded raster is canonical for any future qualification. |
| SAM-only suspect components on page 35; RT misses components on pages 02 and 16 | [fusion summary](../spikes/chapter-combo/results/live-chapter-109-fusion-summary.json) | Unassociated components stay in review; none is silently erased or dropped. |
| COO rock proposal on page 24 has no lettering mask; page 30 has no candidates | [fusion report](../spikes/chapter-combo/FUSED_PIPELINE.md) | No polygon-to-erase conversion and no filled fallback rectangle. |
| Human-label accuracy and reconstruction quality are unmeasured | None | Mask and reconstruction reviews stay separate gates before any automatic or broad release claim. |

## Reproduction and interpretation

The individual reports above contain pinned hashes, commands, source-page
identities, environments and machine-readable results. The local comic scans,
checkpoint files and large prepared tensors live in ignored `artifacts/` and
are not redistributed by this repository. Provider availability is not graph
assignment; graph validation is not mask parity; mask parity is not semantic
accuracy. The benchmark rows describe the tested inputs and host only. The
[provider procedure](provider-qualification.md) specifies the
matching-hardware Windows/Linux measurements still needed, while the [rights decision](model-rights-decision.md)
records what must be resolved before distributing weights.
