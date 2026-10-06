# Independent RT-DETR-v2 and SAM-TS-L capabilities

This implementation is in the `codex/cloud-integration` worktree. The main checkout at `/Users/caved/dev/manga-cleaner` was not edited. Settings and onboarding now select CTD, RT-DETR v2 small or full, and SAM-TS-L in any valid combination for Auto clean and local candidate review. The supervised PNG component write remains limited to the measured M5 WebGPU configuration described below.

## What is wired

| Choice | Files opened | Result |
| --- | --- | --- |
| Regions only | RT full FP32 tiled graph, or existing small INT8 whole-page graph | Text and bubble boxes in page coordinates. Bubble boxes are context. |
| Mask only | SAM encoder and high-resolution text head | Unchanged binary lettering mask and every 8-connected component. No RT, CTD, script gate, COO, or manga-ocr. |
| Text-shaped review | Selected RT graph and SAM pair | Same SAM pixels, plus RT evidence and detector-only candidates. No automatic erase. |
| COO MTSv3 | None | Polygon association API exists in fusion, but UI/runtime import and execution are blocked pending use and distribution rights. No bundle or download. |

The full RT graph and SAM pair have SHA-256 readiness, removal, and local import fallback. Full RT is a managed download of `detector.onnx`, 168,481,531 bytes, SHA-256 `065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195`, revision `16e8a622f91fabc6b5b65c96d32d1183f8843546`. The existing small RT graph is 11,120,765 bytes, SHA-256 `5fe9e4f576e49d4e7e8b0e029d6d3cdc252abd4694113e1cae120e62c931ea79`. SAM checkpoint revision is `5dd97423e0fbf2404264979136d47e8101144046`; setup downloads it and a pinned Hi-SAM source archive, exports the encoder and head locally, then checks their exact expected bytes and hashes. The encoder is 1,335,305,985 bytes, SHA-256 `9b3a32f9018008cfd2c7a5b1a7eb6e20822ba43eab58918863f74ac62ecbafbe`; the head is 22,704,641 bytes, SHA-256 `a2c63ccf54e2e692a281cffd4dcda648f252ae6649dc7d23d0203e5868685281`. This managed export path has not been exercised on Windows or Linux hardware in this worktree.

The native SAM path matches Pillow bilinear RGB preparation, top-left gray-128 padding, the graph's single normalization, tensor names and shapes, zero-logit threshold, padding crop, and Pillow's nearest restoration. The nearest resize uses Pillow's cumulative floating-point coordinate updates; integer center arithmetic changed 161 pixels on page 07. RT full uses the proof's two contiguous vertical tiles, Pillow bilinear 640-square input, width-height target sizes, 0.35 score threshold, and source-coordinate restoration. Fusion retains all SAM components and all RT text proposals, including detector-only ones. COO polygons can suggest fragment groups but cannot create erase pixels.

## Regression and measurements

The local saved Apocalypse 109 run verifies 45/45 original source SHA-256 hashes, 45/45 unchanged SAM mask hashes, the exact saved RT box lists and COO detection files, 922 SAM component candidates, and four detector-only candidates with no mask label. Page 02 keeps seven SAM components and two SFX grouping suggestions despite no RT box; page 16 keeps two SAM-only components; page 24's COO rock proposal has no erase mask; page 30 has no candidates; page 35 keeps four suspect SAM-only components for review. These are candidate and geometric checks, not accuracy against human labels.

Native comparisons used ONNX Runtime 1.28.0 CPU on Apple M5 / macOS, with JPEGs decoded to temporary RGB PNGs by the pinned Pillow reference. The 45-page full RT path reproduced every reference class and box coordinate; scores differed by less than `1e-4`. Native SAM masks were byte-identical on 44/45 pages; page 27 differed by **two pixels**. The [page-27 stage diagnosis](../spikes/sam-ts-l/results/native-page27-parity.md) found an identical Rust/Python prepared tensor and identical ONNX masks on shared input. The two changed pixels map to one near-zero canvas logit whose sign differs between PyTorch and ONNX Runtime; disabling or reducing ONNX graph optimization did not change that sign. This is a specific numerical blocker to exact PyTorch mask parity, not a preprocessing or restoration mismatch. The saved Python fusion run itself retains all 45 original mask hashes unchanged.

The desktop defines its input as the app-decoded `Raster`. Its JPEG decode differs from Pillow in 104,342 RGB channel samples on page 27 and changes 666 ONNX mask pixels; therefore the Pillow-decoded JPEG run cannot be an exact baseline for the desktop's original-JPEG path. Python and native ONNX Runtime produce identical masks when both receive a PNG of the app-decoded raster. Original-JPEG Pillow parity remains **unqualified**; no threshold adjustment or invented pass limit was used. The component write path accepts PNG only and requires the measured M5 WebGPU backend.

| Native CPU path | Graph bytes | Cold session load | Median page time | Peak process RSS | GPU memory |
| --- | ---: | ---: | ---: | ---: | --- |
| Full RT, two tiles | 168,481,531 | 201 ms | 732 ms including prep | 665,714,688 bytes | N/A; CPU requested |
| Existing small RT, whole page | 11,120,765 | 93 ms | 86 ms including prep | 622,870,528 bytes | N/A; CPU requested |
| SAM encoder + head, sequential sessions | 1,358,010,626 | 604 ms median per page load | 5,723 ms encoder + head; 6,327 ms including load | 8,547,975,168 bytes | N/A; CPU requested |

Cold load is the first session build in the separate native test process (RT 201 ms; SAM first page 534 ms); the SAM table also gives the median repeated per-page load because the implementation releases the large session after each page. The RT test took 39.51 seconds wall including compilation/test setup. SAM took 310.39 seconds wall and includes one long page-44 outlier. RSS is `/usr/bin/time -l` process high-water and is not a system or GPU-memory bound.

The app-downloaded ORT 1.28.0 dylib (SHA-256 `dc19bbcb2f5c9fb3c68b4f9248aa0a35065ff702c5dbeae75eac54a74da97b6d`) exposed built-in WebGPU on MacBook Air Mac17,3 / Apple M5 10-core / 32 GB / macOS 27.0. Native Rust assigned all 1,818 encoder and 386 head profiled node events per run to WebGPU, zero to CPU. Across 45 identical Pillow-prepared inputs, it matched 45/45 saved PyTorch MPS restored masks with zero changed pixels. One-session median encoder+head+raw-write was 6,303 ms (range 5,919–6,949 ms); cold encoder/head load was 689/37 ms; peak process RSS was 2,475,360,256 bytes; 20 ms sampled Metal high-water was 5,948,866,560 bytes. A focused desktop `infer_webgpu` test with a PNG version of page 07 also matched its saved mask and assigned both full graphs to WebGPU. These results do not qualify app-decoded original JPEGs. The Metal sample can miss a transient peak and overlaps RSS in unified memory. See [provider qualification](provider-qualification.md) for files and commands. Windows and Linux CPU/GPU rows remain unqualified and unselectable until matching hardware is tested.

## Commands used

```sh
cargo check -p manga-cleaner --quiet
cargo test -p cleaner-core --quiet
cargo test -p manga-cleaner --quiet
npm run build
npm test -- --reporter=dot
npx vitest run src/lib/dialogs/WorkflowAnalysis.dom.test.js --reporter=dot
python3 spikes/chapter-combo/verify_regression.py
git diff --check
```

The ignored native graph tests used `SAM_TS_RUNTIME` or `RT_RUNTIME` pointing to the local ORT 1.28.0 dylib, `RT_DECODED_DIR` / `SAM_TS_DECODED_DIR` pointing to temporary Pillow-decoded PNGs, and the local ignored proof graphs. See `rt_regions.rs` and `sam_ts.rs` for their exact environment variables. The full RT test passed; the strict full SAM test fails on page 27 by two pixels, deliberately keeping the parity gate visible.

## Files changed for this capability work

- Workspace and graph dependencies: `Cargo.toml`, `Cargo.lock`, `src-tauri/Cargo.toml`.
- Native inference and fusion: `crates/cleaner-core/src/{lib.rs,balloon.rs,sam_ts.rs,rt_regions.rs,fusion.rs}`, `src-tauri/src/{lib.rs,model_workflows.rs}`.
- API and workflow UI: `src/lib/api/{backend.js,folder.js,mock.js,tauri.js}`, `src/lib/model/pipelines.js`, `src/lib/i18n/en.js`, `src/lib/dialogs/{SettingsDialog.svelte,WorkflowAnalysis.svelte,WorkflowAnalysis.dom.test.js,SettingsDialog.models.dom.test.js,FirstLaunchDialog.dom.test.js}`.
- Chapter verification and evidence: `spikes/chapter-combo/verify_regression.py`, this file.

Other modified files already present in the worktree are the separate M0–M1/cloud work and were preserved.

## Release gates

The [component write contract](component-write-contract.md) limits supervised writes to explicit WebGPU analysis of PNG on the measured M5 host with the pinned app runtime. The analysis and prepared plan pin backend and runtime hash; prepare/apply recheck them, source, support, and composited underlay. Native CPU review remains 44/45 exact and cannot prepare a write. JPEG, longstrip, and other hardware remain review-only. Human-labeled pixel and instance ground truth is required before any accuracy improvement claim. Windows/Linux need matching-hardware provider tests. RT and SAM weight distribution terms must be confirmed before bundling their graphs. The [rights decision](model-rights-decision.md) excludes COO from desktop import, execution, bundling, and downloads; this is a product exclusion, not rights clearance. No paid cloud job was started.
