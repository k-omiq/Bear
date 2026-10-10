# Native local detection smoke — 2026-10-09

The installed native route successfully ran CTD, Ogkalu Full and SAM-TS-L on a full synthetic page and a selection. Both CPU SAM and Apple WebGPU SAM returned nonempty detection masks; their results could be stored and reopened with unchanged masks, ink and model evidence. No page upload or cloud job was used.

This is a functional smoke, not an accuracy study or a release performance benchmark. It used the Rust debug/test build while other repository validation was running. The All text policy was enabled and the optional OCR reader was disabled; the model combination, grouping, area crop and persistent detection outputs were exercised.

## Machine and inputs

- macOS `Mac17,3`, Apple M5, 32 GiB physical memory.
- Installed ONNX Runtime reports `1.28.0` through `OrtGetApiBase`.
- Runtime: `~/Library/Application Support/com.mangacleaner.studio/runtimes/libonnxruntime.dylib`; SHA-256 `dc19bbcb2f5c9fb3c68b4f9248aa0a35065ff702c5dbeae75eac54a74da97b6d`.
- Fixture: `spikes/sam-ts-l/fixtures/synthetic_page.png`; SHA-256 `55d90fd5daf532b26dbd347ed5f0de82cf1461e1ed1c10bbecf04aad457d9a11`.
- Selection: `{x: 0, y: 0, w: 75, h: 75}` in page percentages, using the production detection context window.
- CTD and Ogkalu Full used CPU in both runs. SAM used the explicitly selected CPU or Apple WebGPU route. WebGPU inference must build without CPU fallback and verify all executed encoder/head nodes before returning.

Installed graph hashes matched the pinned manifests:

| Graph | SHA-256 |
| --- | --- |
| Ogkalu Full `detector.onnx` | `065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195` |
| SAM encoder `koharu_samts_encoder.onnx` | `9b3a32f9018008cfd2c7a5b1a7eb6e20822ba43eab58918863f74ac62ecbafbe` |
| SAM text head `koharu_samts_text_head.onnx` | `a2c63ccf54e2e692a281cffd4dcda648f252ae6649dc7d23d0203e5868685281` |

## Observed results

Each backend ran in a fresh test process. The first full-page measurement includes opening all three model sessions, inference and grouping. Selection then reused those same sessions. These are total native detection durations under concurrent test load, not isolated GPU inference timings.

| SAM backend | Full page, first sessions | Full-page regions | Selection, reused sessions | Selection regions |
| --- | ---: | ---: | ---: | ---: |
| CPU | 44.55 s | 2 | 27.78 s | 2 |
| Apple WebGPU | 39.86 s | 2 | 18.35 s | 2 |

Both backend tests passed all of the following assertions:

- Ordinary full-page and selection inference produced nonempty region lists and detection masks.
- Mask and ink pixel counts were nonzero.
- Group evidence decoded, its SHA-256 reference matched, and rebuilding group lettering reproduced the recorded pixel count.
- Every contributing model recorded local execution.
- Selection inference produced no cleaned page patches.
- Each output set was added to a scratch `.mtclean` job, then reopened. Masks and ink matched exactly, and stored evidence remained readable. The manifest held two detections after the full-page output and four after appending the selection output.

The scratch append validates the persistent output contract; the ordinary-run scheduler's replacement behavior is covered by its separate regression suite. This smoke does not compare CPU and WebGPU mask accuracy, qualify another host/runtime, run a real chapter, or qualify direct preview component writes.

## Reproduction

The test is ignored by default because it loads the large installed models. Use the app's installed runtime and a model directory holding CTD, Ogkalu Full and the `sam-ts-l` subdirectory:

```sh
SAM_TS_RUNTIME="$HOME/Library/Application Support/com.mangacleaner.studio/runtimes/libonnxruntime.dylib" \
MANGA_CLEANER_MODELS="$PWD/models" \
SAM_TS_BACKEND=cpu \
cargo test -p manga-cleaner --lib installed_local_detection_pipeline_runs_full_page_and_selection \
  -- --ignored --nocapture --test-threads=1
```

Repeat with `SAM_TS_BACKEND=webgpu` for the supported Apple WebGPU route. Local SAM loading requires at least 10 GB of measured memory room after idle sessions are released. A shared permit keeps only one SAM graph pair loaded at a time; a queued request can cancel while waiting, and cancelling a native run terminates its SAM/Ogkalu inference options.
