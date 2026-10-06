# Execution controls and capability matrix (2026-09-26)

## Control inventory and routing

Settings > Detection and first-launch setup select CTD, small or full RT-DETR v2, and SAM-TS-L. Small and full RT-DETR are mutually exclusive. The text policy selects language filtering or all-text analysis; the Japanese OCR rescue is optional. Auto clean receives those selections with the reconstruction choice, solid-fill color, and outside-bubble policy from the editor. Settings > Cleaning selects the FLUX model/backend. Settings > Performance holds a global local backend default and per-model overrides for CTD, both RT profiles, SAM-TS-L, and LaMa Manga. An absent override inherits the global default. The choice persists in the session and `settings.json` and is read when the next native session opens. Settings reports predicted placement separately from actual loaded-model device reporting.

Cloud reconstruction is selected as a named ☁ FLUX model in the editor. ☁ SAM-TS-L and ☁ RT-DETR v2 full are available through explicit, consented page Review analysis on the configured endpoint. Cloud analysis produces review evidence; Auto clean runs locally and cannot apply cloud analysis as a component write.

Backend status terms have distinct meanings: **supported** means a native path exists for that model and OS; **installed** means the required provider is present in the selected runtime; **available** additionally requires its dependency and device probes to pass; **verified** means actual model/provider inference evidence exists for this tested host. A supported backend on another OS is not thereby verified. Forced unavailable choices stop the next run, while Automatic may choose another provider and reports the reason.

## Model × OS × device × backend

`CPU` is the ONNX CPU runtime. The other entries are GPU providers; availability still depends on the selected ONNX runtime build, drivers, vendor libraries, and model files. “Path” means implemented and selectable when available; it does not claim a successful installation or inference on that OS.

| Model | macOS arm64 | Windows x64 | Linux x64 | Cloud GPU |
| --- | --- | --- | --- | --- |
| Comic Text Detector (CTD) | CPU; WebGPU verified on M5. CoreML failed strict graph placement | CPU; DirectML, CUDA and WebGPU paths, GPU unverified | CPU; CUDA and WebGPU paths, GPU unverified | No cloud CTD analysis |
| RT-DETR v2 small INT8 | CPU verified on M5. WebGPU and CoreML fail strict graph placement | CPU; DirectML and CUDA paths, GPU unverified | CPU; CUDA path, GPU unverified | No small-RT cloud profile |
| RT-DETR v2 full FP32 | CPU verified on M5. WebGPU and CoreML fail strict graph placement | CPU; DirectML and CUDA paths, GPU unverified | CPU; CUDA path, GPU unverified | CUDA, explicit Review only |
| SAM-TS-L | CPU; WebGPU verified on M5 with full graph assignment and reused session | CPU; no supported local GPU path | CPU; no supported local GPU path | CUDA, explicit Review only |
| LaMa Manga | CPU verified on M5; WebGPU path, strict assignment unverified | CPU; DirectML and WebGPU paths, GPU unverified | CPU; WebGPU path, GPU unverified | No cloud LaMa |
| FLUX.2 Klein local helper | MLX (`mflux`) on Apple Silicon; SDNQ with Metal | SDNQ with CUDA or Intel XPU | SDNQ with CUDA or Intel XPU | Separate cloud FLUX choices on configured endpoint |

MLX in this table is the real `mflux` implementation for FLUX. It is not an ONNX provider for CTD, RT-DETR, SAM or LaMa. The FLUX helper's SDNQ runtime is separate from ONNX Runtime and needs a compatible GPU; its availability is checked during setup/open rather than inferred from the ONNX provider list. DirectML, CUDA and WebGPU are shown only for native model paths that can request them. TensorRT, ROCm, OpenVINO and XNNPACK are not offered for these model paths merely because a runtime lists them. A CUDA provider is available only when CUDA runtime, cuDNN 9 and an NVIDIA adapter are present; this is still not a claim of verified inference on Windows/Linux.

## Validation scope

The local development host is the Apple M5 Mac described in [Windows/Linux installation validation](windows-linux-install-validation-2026-09-26.md). Real synthetic inference passed for CTD WebGPU, SAM-TS-L WebGPU twice on one session with zero CPU node fallback, and small/full RT CPU. Earlier native inference records also cover SAM CPU and LaMa CPU; LaMa WebGPU timing records do not establish strict full graph assignment, so its `verified` flag remains false. Forced RT WebGPU and CoreML plus CTD CoreML failed strict graph assignment and are not offered on macOS. Native GPU execution on Windows and Linux remains unverified because this workspace has neither OS with a GPU. The Windows and Linux package/installation boundary is recorded in that report. Live Modal identities, timings, provider assignment, usage and cleanup are recorded in [the cloud validation report](cloud-live-modal-2026-09-26.md).

The native validation used the pinned macOS ONNX Runtime 1.28.0 library (`sha256: dc19bbcb…b6d`) and the production model graph hashes. The final `cargo test -p manga-cleaner --lib` run passed 557 tests (3 ignored); `cargo test -p cleaner-core accel::tests --lib` passed 24 (1 manual test ignored). The M5 manual inference checks `native_webgpu_detector_runs_without_cpu_nodes`, `native_webgpu_session_reuses_verified_assignment`, `native_cpu_full_graph_runs_synthetic_page`, and `native_cpu_small_graph_runs_synthetic_page` passed. The SAM check ran two inferences on one persistent session, compared identical masks and found zero CPU fallback nodes. These establish the named model/backend paths on this machine; Windows/Linux GPU paths and a clean installed desktop flow remain unverified.
