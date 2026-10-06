# Modal cloud validation — 2026-09-26

This report records a temporary installation in the existing `k-omiq` Modal workspace, environment `main`, on macOS 27.0 (26A428), Apple M5. The temporary app was `mc-live-20260926` (Modal app `ap-pdv9uw0ONfpYmq4YW9TOlh`), using an L4 GPU, 60-second idle timeout, FLUX.2 Klein 4B, and both review analysis graphs. Modal SDK version was 1.5.5. No credential values or runtime token identifiers are recorded here.

## Execution paths and model identity

- Account inspect, plan, apply, resume, and cleanup used the application's `python -m provisioner` helper protocol with explicit credentials from the selected installed Modal profile. These were actual provisioner code paths. The host did not have a saved desktop `inference.json`, so live Tauri command/UI installation was not exercised.
- Most inference requests used authenticated HTTP to the deployed gateway with the same wire formats as the app. They were not sent through Tauri commands. An ignored environment-gated Rust `CloudHttpClient` test separately exercised the native transport: authenticated health, model info, capabilities, and a 16 × 16 RT tile all passed in 25.72 s. This used the app's endpoint validation, credential binding, DNS/TLS transport, request digest, and response validation, but not the desktop Tauri command/UI flow.
- Unauthorized health returned HTTP 401. Authenticated health, model-info, and analysis capabilities returned HTTP 200. The production model was `Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic`, revision `45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd`, recipe `mc-flux2-klein-edit-v1`.
- Analysis advertised independent `text_mask_sam_ts@1` (revision `5dd97423e0fbf2404264979136d47e8101144046`) and `text_regions_rt@1` (revision `16e8a622f91fabc6b5b65c96d32d1183f8843546`) capabilities. These are review tile operations; Auto clean was not routed to cloud analysis.

## Live inference and recovery

| Probe | Observed result |
| --- | --- |
| RT-DETR cold, 96 × 96 synthetic RGB tile | HTTP 200, valid response, 2 boxes, 0 components, 22.425 s wall time; reported 6,023 ms load and 1,755 ms inference. |
| RT-DETR warm, same tile | HTTP 200, valid response, 3.533 s wall time; reported 0 ms load and 13 ms inference. |
| SAM-TS-L warm, 96 × 96 tile | HTTP 200, valid response, 6 components, 159-byte valid mask PNG, 4.644 s wall time; reported 2,283 ms load and 579 ms inference. |
| FLUX cold, 32 × 32 synthetic edit | Job accepted and completed in 106.8 s; result was a valid RGB 32 × 32 PNG, 516 bytes, with matching digest header. |
| FLUX warm, same small format | Completed in 5.46 s with a matching PNG digest. |
| FLUX cancellation | Cancel request acknowledged; job reached `cancelled` in 5.48 s. A following render completed in 5.94 s with a valid digest. |
| Invalid image digest | HTTP 400 `invalid_digest`, `enqueued=false`, `retryable=false`; authenticated health remained HTTP 200. |
| FLUX after idle shutdown | The prior L4 worker disappeared after idle timeout. A new GPU worker started for the next 32 × 32 render, which completed in 52.31 s with a valid PNG digest. |

The analysis worker ran on NVIDIA L4. ONNX Runtime reported TensorRT, CUDA, and CPU providers available. Profiling with the app's torch/CUDA libraries loaded showed 1,030 CUDA and 417 CPU node events for RT; the CPU events were shape/support operations. SAM encoder had 1,482 CUDA node events and its text head had 314 CUDA events. A standalone ORT probe without loading torch first could not resolve `libcublasLt.so.12` and fell back to CPU; the deployed app imports torch before ORT and the live worker/profiling used CUDA. Thus RT is mixed CUDA/CPU execution, while the measured SAM graph events were CUDA.

The first live seed exposed two deployment defects: the SAM text-head ONNX export had a different deterministic SHA-256 on Linux (`d9431cf1828bbbf70db26f438f5b5777783729dbf7cac5f20cd2b14057125bd2`) than on macOS, and RT's Hugging Face `local_dir` copy onto a Modal volume raised `SameFileError`. The pinned Linux export is 22,704,641 bytes, with the same graph structure and initializer names as the macOS export. ONNX Runtime 1.23.2 CPU fixed-input comparison against the reference mask showed maximum absolute difference `1.907e-05`, mean absolute difference `5.276e-06`, and zero threshold disagreements. A first RT gateway request then exposed missing Pillow in the CPU gateway image. The deployment was updated with pinned platform hashes, off-volume RT downloads, and Pillow in the gateway image; subsequent requests above passed. Resume completed without duplicate resources.

## Billing and resources

The gateway, job status/result, and analysis responses reported per-request cost as `null`/unknown. A read-only helper billing query for cycle `2026-09` returned workspace-wide metered cost **USD 0.31** and billed cost **USD 0.00** at query time. This is workspace-wide, potentially delayed billing, and cannot be assigned to these individual probes. The plan now discloses the separate seed allocation: when SAM is selected, 2 CPU and 24 GiB for up to 2 hours per attempt, with retries charged while active; without SAM, 2 CPU and 4 GiB for up to 1 hour per attempt. GPU worker, warm idle, gateway, and volume storage remain separately disclosed in the plan.

## Verification boundaries

- Python cloud suite: 213 passed, 13 skipped. Provisioner suite: 142 passed, 7 skipped. Modal SDK offline import suite: 3 passed. Focused seeding tests covered platform hash selection and RT download placement. The ignored live Rust `CloudHttpClient` test passed with one RT tile.
- No live Beam deployment, desktop Tauri command/UI flow, or production user image was tested. The HTTP probes demonstrate the deployed gateway and models; the native `CloudHttpClient` probe demonstrates the Rust app transport path only to the extent stated above.
- The helper cleanup plan listed the temporary proxy token, app, job Dict, and weights Volume. Cleanup apply reached `cleaned_up`; a second cleanup plan listed zero remaining resources. Modal CLI showed no Dict or Volume, and the app was stopped at 10:08:02 Asia/Dhaka. The existing `k-omiq` profile remained active and its credentials were left intact.
