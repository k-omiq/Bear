# M0 evidence for M1, and M1 verification

Recorded on `codex/cloud-integration` from commit `bceb56900164adcc95155ce3ef979d67199d3b98`. The pre-existing unstaged `docs/findings.md`, `docs/phased-implementation-plan.md`, `docs/repeated-inpaint-plan.md`, and `Plan for models/` were preserved.

## Reproducible starting point

| Item | Observation |
| --- | --- |
| Host | macOS 27.0 build 26A428, arm64, 32 GiB physical memory |
| Tools | Rust/cargo 1.96.0; Node 26.3.0; npm 11.16.0 |
| Project | `.mtclean` format version 3; versions 1–3 readable (`crates/cleaner-core/src/project/mod.rs`) |
| CTD | `comictextdetector.pt.onnx`, catalogue SHA-256 `1a86ace74961413cbd650002e7bb4dcec4980ffa21b2f19b86933372071d718f` |
| RT-DETR-v2 | `detector-v4-s_int8.onnx`, catalogue SHA-256 `5fe9e4f576e49d4e7e8b0e029d6d3cdc252abd4694113e1cae120e62c931ea79` |
| LaMa | `lama-manga.onnx`, catalogue SHA-256 `4512adab295ee5a5e02ccd1bdf8d45dccbac88309d9cff1532ffd5de876f02a4` |
| ONNX Runtime | Rust `ort`/`ort-sys` 2.0.0-rc.13. Installed app runtime `libonnxruntime.dylib` SHA-256 `dc19bbcb2f5c9fb3c68b4f9248aa0a35065ff702c5dbeae75eac54a74da97b6d`. Runtime provider execution was not measured in this slice. |
| Installed weights | `~/Library/Application Support/com.mangacleaner.studio/models` is a symlink to `/Users/caved/dev/manga-cleaner/models`. Rechecked on 25 September 2026: the CTD, RT-DETR-v2 INT8 and LaMa files there match the catalogue SHA-256 values above; the script-identification pair and the three manga-ocr files are also present. `MANGA_CLEANER_MODELS` was unset. The worktree's own `models/` holds no weights. |

The starting tests passed: `cargo test -p cleaner-core` (571 unit tests plus integration/doc tests), `cargo test -p manga-cleaner` (451 native tests), and `npm test -- --run` (57 files, 1133 tests). No real-page recall, model quality, OCR, provider speed, or cross-platform performance figure was measured here. The M0 sample corpus and rights ledger for later model comparisons remain to be assembled; the synthetic regression below is rights-cleared and deterministic.

## Failure and correction

The `region::tests::a_later_ai_stroke_reads_the_visible_first_stroke` fixture writes black lettering on a white synthetic scan, commits a white A patch, and captures the actual raster and fitted mask handed to B's fake renderer. Before the fix it failed with B reading `(0, 0)` at the sampled overlap/context pixels, where the visible composite was `(255, 255)`. This classifies the defect as **late edit reading raw source**, separate from discovery, policy, segmentation, and inpainting quality.

The same fixture now checks overlap, neighboring context, retry B below its stored order (excluding B and newer C), hidden A, and dependency review. The strip fixture checks both sides of a join and cloud preparation from a second page's predecessor. Consent and attachment tests reject changed predecessors without a second attempt or target mutation. The frontend tests check rapid gesture order, page switching, cloud creation as one undo step, and consent cancellation. A compositor/export test checks, for every built-in source mode/depth fixture, that each pixel outside B's write mask equals its immediate A underlay and the decoded lossless export equals the preview composite.

The final verification run passed `cargo test -p cleaner-core` (571 passed, one existing ignored unit test; integration and doc tests passed), `cargo test -p manga-cleaner` (455 passed, one benchmark ignored), `npm test -- --run` (57 files, 1141 tests), and `npm run build`. The benchmark below was run separately. These are synthetic and existing repository tests, not a model-backed image-quality comparison.

Manual AI operations now compose one bounded visible underlay and use it for fitting, edges/noise, rendering, quality, and patch creation. Cloud proposal, confirmation, dispatch, and attachment recompute the same encoded crop and hint with exact input/predecessor hashes. The dispatch-time comparison was missing when this was first written: the grant's input and predecessor hashes were checked at consent and attach but not at dispatch, so a change inside the read window could still send a paid job that could only fail at attach. It was added on 25 September 2026 and is covered by `changed_underlay_refuses_dispatch_without_spending_grant`, `changed_predecessor_inside_read_window_refuses_paid_dispatch_and_preserves_grant` and `predecessor_change_mid_poll_stales_one_paid_job_without_attaching` (one submission, nothing attached, result kept cached). The source-file hash remains separate. A changed earlier layer marks a later layer for review; Keep, Rebuild, and Undo are exposed without automatic recomputation or paid submission.

## Bounded memory

The ignored benchmark `cargo test -p manga-cleaner bounded_4000x6000_dense_history_benchmark -- --ignored --nocapture` ran a 4000×6000 8-bit synthetic page with 96 overlapping stored patches on this macOS host. It reported `raw=24000000`, `window=2611456`, `patches=96`, `peak_rss=43401216` bytes, `read_ms=259`, and `total_ms=2797` including fixture construction. The composite read ceiling is 4096×4096 pixels; larger requests fail visibly. This is a process measurement for one grayscale fixture, not a target budget for RGB, GPU providers, or production page histories. No underlay cache was added: the 2.6 MB read buffer and 259 ms read did not justify retaining additional history rasters in M1.

M2's SAM-TS-L **mask-only ONNX export research can begin** using Addendum 09's pinned reference and strict-loading requirements. This M1 result does not establish model parity, artifact redistribution rights, or a production ONNX provider choice.
