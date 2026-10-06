# Optional local text-shaped workflow: release evidence and rollback

Status: M4–M6 implementation review, 25 September 2026. This document describes the local boundary only. It does not authorize cloud detection, COO MTSv3, Qwen, or distribution of converted RT/SAM weights.

Current implementation note, 26 September 2026: Settings and onboarding now support independent CTD, RT-DETR v2 (small or full), and SAM-TS-L choices in Auto clean as requested, with managed full RT download and local SAM export. The historical M5/M6 decisions and measurements below describe the earlier optional review path. They do not validate cleaning quality or exact graph reproduction on every operating system; no cross-platform quality promotion is implied.

## User workflow and write boundary

Legacy Auto Clean remains the default for old and new projects. Its source-language choices are captured when a run starts and restored on resume; Skip holds that language's candidates, and Japanese OCR rescue is an explicit option. The script identifier cannot reliably separate Japanese kanji from Chinese Han, so a Han result remains eligible when either Japanese or Chinese is selected. A run with every language skipped performs no cleaning. Outside-bubble opt-in deliberately bypasses script reading; when only some languages are selected, such outside candidates are held for review because their language cannot be checked against Skip. With all languages selected, the opt-in cleans them.

The optional **Text-shaped review** uses a selected library chapter page. RT-DETR supplies candidate boxes; the pinned Koharu SAM-TS-L encoder and text head supply lettering pixels and retain unassociated components for review. This all-text analysis does not open the script gate or manga-ocr. COO is excluded under [the rights decision](model-rights-decision.md). A selected component can be prepared for a supervised write only on the [qualified PNG/M5/WebGPU combination](component-write-contract.md). CPU, JPEG, longstrips, unmeasured hosts, and missing or failed models remain review-only.

The tinted raster is the exact approved write support **W**. It is derived from immutable SAM component pixels plus explicit add/remove correction and source-pixel round padding. The numerical default is **0 px**; the engineering range is **0–64 px**. These are conservative interface bounds, not a quality-tuned optimum. The model's zero-logit threshold, no implicit fragment grouping, no automatic glow expansion, and no extra crop overlap remain the reference decisions pending a labeled holdout. Bbox is only a locator. Preparation and apply bind source, model, provider/runtime, lower composite, plan revision, and support hash; a stale or unsafe plan cannot commit. Reconstruction may read more context, but the stored patch and final compositor can write only W.

Individual prepared writes and text-shaped revisions are persisted with versioned mask artifacts. Selecting a previously edited component reloads its saved add/remove correction raster, padding, and correction revision before another preview. Changing padding from 2 to 5 to 2 always derives from the immutable base. Undo and redo select exact archived patch revisions. A saved patch stays viewable if the optional mode is disabled. Disabling a feature in this build is the supported rollback; opening a v4 manifest in an older binary has not been qualified. Formats v1–v3 open with legacy geometry defaults, and an unknown future geometry mode fails visibly.

Related script gate and OCR files are one logical capability each in the Models interface. Their component names, sizes, and pinned SHA-256 values remain inspectable. These older catalogue entries have no recorded upstream revision; the file digest is their exact artifact identity. Group install stages and verifies missing files before activation; a failed transfer leaves existing installed files intact. Removing Japanese filtering does not remove shared RT-DETR weights.

## Measured resources and quality limits

The [model workflow benchmark record](model-workflow-benchmarks.md) compares
the earlier controlled PyTorch/ONNX page-07 probes, full-chapter SAM and RT
runs, and live three-model research fusion. Their timing and memory scopes
differ from the integration fixtures below.

| Sample | Observation | Limit |
| --- | --- | --- |
| 4000×6000 Gray8 M3 page + 128×128 prepared mask, macOS M5 | 24,000,000 decoded page bytes; 33,701,888 B process RSS high-water; 26,804,704 B reported peak footprint | No ONNX, renderer, or PSD in this measurement. |
| 4000×6000 synthetic white L8 SAM-TS-L WebGPU run, macOS M5, app ORT 1.28.0 | 15.879 s total; 2,625,880,064 B process RSS high-water; 5,903,499,264 B Metal high-water sampled at 20 ms | One synthetic page. Metal sampling can miss transients and overlaps RSS in unified memory. |
| Saved 45-page Korean color webtoon chapter (Apocalypse 109), identical Pillow-prepared input and measured M5 WebGPU | 45/45 restored masks match saved PyTorch MPS masks; zero changed pixels | Mask parity, not human-label accuracy or reconstruction quality. |
| Native CPU on Pillow-decoded chapter | 44/45 exact restored masks; page 27 differs by two threshold pixels | CPU remains a review path. |

A local real-graph WebGPU integration fixture also passed native SAM inference → component preparation → approved hash apply → lossless export, with every changed decoded pixel inside W. This validates the write bridge on the measured host; it does not measure reconstruction quality.

No rights-cleared, untouched Japanese/Korean human-labeled holdout with instance, complete-page, text-pixel, artwork-damage, false-candidate, and correction-time measures has been assembled. M5 quality promotion and M6 release claims therefore remain **no-go**. Windows and Linux providers have no matching-hardware measurement. The current artifact rights decision permits local, hash-pinned user import, not app-hosted RT/SAM downloads or bundled weights.

## Known-failure gallery

| Case | Reproducible artifact | Product response |
| --- | --- | --- |
| Near-zero logit changes two CPU mask pixels on page 27 | [stage diagnosis](../spikes/sam-ts-l/results/native-page27-parity.md) | CPU review only; no exact-parity write approval. |
| Original JPEG decoding changes SAM input and 666 mask pixels on page 27 relative to Pillow | [model capability evidence](model-capabilities-evidence.md) | JPEG review only; the native decoded raster is canonical for any future qualification. |
| SAM-only suspect components on page 35; RT misses components on pages 02 and 16 | [fusion summary](../spikes/chapter-combo/results/live-chapter-109-fusion-summary.json) | Retain unassociated components in review; do not silently erase or drop them. |
| COO rock proposal on page 24 has no lettering mask; page 30 has no candidates | [fusion report](../spikes/chapter-combo/FUSED_PIPELINE.md) | No polygon-to-erase conversion or filled fallback rectangle. |
| Human-label accuracy and bad reconstruction are unmeasured | [M0/M1 evidence](m0-m1-evidence.md) | Require mask and reconstruction reviews as separate gates before an automatic or broad release claim. |

## Exit decision

- **M4:** **Go for local M5 validation.** The selected chapter review uses native analysis and exact W prepare/apply; synthetic native and frontend/API tests cover policy Skip, correction reload, hash approval, keyboard and zoom/DPR pointer correction, grouped setup, and revision undo/redo. This decision does not qualify Windows WebView2 preview memory or a general release.
- **M5:** local ONNX integration and fixed-corpus parity exist; human-labeled quality and non-M5 providers are unverified. **No go for automatic text-shaped cleaning.**
- **M6:** optional supervised PNG/M5/WebGPU component write is narrowly qualified; hosted artifacts, cross-platform providers, and general release quality remain unqualified. **No go for a broad local release.**
