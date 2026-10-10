# Denoise submission gaps on yarafiq1971 — 2026-10-09

This report records the read-only investigation, before the [queue and result-transfer fix](denoise-queue-fix.md). During that investigation, no runtime code, installed application, deployment, cloud job, or GPU settings were changed. Read existing logs, existing provider job documents/results, local attempt journals, and three-second samples of the running application. A later authenticated GET measured the transfer of an already-completed result for 40 seconds; it submitted no inference and retained only timing/byte-count metadata. Times below are EDT.

## Conclusion

The main delay is retrieving completed denoise results. It is not multi-minute GPU inference. A live authenticated read received only 69,626 bytes in 40 seconds, with the first body data at 5.932 seconds. Denoise embeds a whole base64 PNG in its completed status response, reads it under a 90-second request limit, and discards partial downloads on failure. Five consecutive failed polls can occupy each worker for about eight minutes. Both denoise workers hold their slots throughout this wait, so later pages remain unsubmitted and the GPU becomes idle between pairs.

The recent queue changes improved bulk cleaning and selection detection. They did not replace denoise's two-worker, complete-round-trip scheduler, and render's resumable result download is not used for denoise.

There is also a distinct reason two pages were not sent: pages 6 and 9 were edited after the batch snapshot. The appearance check rejects changed pages before upload. That behavior preserves the approved input, but failure reasons are not shown individually until the entire batch ends.

## Evidence from the affected installation

Modal account/profile: `yarafiq1971`. App: `mc-unbswc`. Desktop label: `DN+QWEN`. Deployment options record L40S, 120-second idle window, denoise enabled, and `us-east` routing. Chapter: `c79`, 1125 × 1600 RGB8 pages.

The read-only provider snapshot contained 15 matching denoise job documents, all `completed`, without provider errors. The corresponding local records contained only three `run_page_complete` outcomes; the others were unknown or still waiting at the time of observation. Match used the request digest, not submission order: the two workers can submit a page pair in either order.

“Provider marked complete” is the gateway's first durable observation of the completed call, not an exact GPU execution-end timestamp.

| Page | Provider accepted | Provider marked complete | Local outcome recorded | Local outcome |
| --- | --- | --- | --- | --- |
| 1 | 15:28:04 | 15:28:34 | 15:36:17 | unknown remote state |
| 2 | 15:28:03 | 15:28:32 | 15:36:13 | unknown remote state |
| 3 | 15:36:18 | 15:36:38 | 15:44:22 | unknown remote state |
| 4 | 15:36:21 | 15:36:37 | 15:37:31 | completed locally |
| 12 | 15:57:16 | 15:57:42 | 16:05:26 | unknown remote state |
| 13 | 15:57:13 | 15:57:44 | 16:05:26 | unknown remote state |
| 14 | 16:05:30 | 16:05:48 | 16:10:49 | unknown remote state |
| 15 | 16:05:30 | 16:05:45 | 16:12:05 | unknown remote state |

For example, the page 12 handle was already completed by 15:57:42. Its later status GETs logged total/execution durations of:

- 15:59:09: 90.0 s / 4.35 s.
- 16:00:42: 89.4 s / 5.25 s.
- 16:02:15: 85.0 s / 4.04 s.
- 16:03:48: 87.9 s / 4.71 s.
- 16:05:21: 83.5 s / 4.03 s.

The app then sent a cancel request and marked the local attempt unknown. The provider document remained completed: requesting cancellation does not undo an already completed call. A successful provider HTTP log with status 200 does not establish that the desktop received the complete response body.

The process sample caught both `denoise_one` threads inside `HttpGpuJob::status → job_request → read_bounded_body → reqwest::blocking::Response::read`, parked waiting for response-body data. It did not catch them in local PNG preparation or color restoration during this sample.

Read three already completed outputs using `FunctionCall.get(timeout=0)`; this did not submit new inference. Their metadata was:

| Page | Model step time | Raw result PNG | Base64 payload alone | Existing-output SDK read |
| --- | --- | --- | --- | --- |
| 2 | 7.116 s | 2,263,132 bytes | 3,017,512 bytes | 7.186 s |
| 4 | 7.121 s | 2,237,614 bytes | 2,983,488 bytes | 7.111 s |
| 13 | 4.937 s | 2,273,355 bytes | 3,031,140 bytes | 8.164 s |

All returned dimensions were 1125 × 1600, `gray: false`, with the expected Real-CUGAN conservative model. The SDK reads occurred later and through a different transport; their durations are not a controlled comparison of the affected HTTP path during the screenshot interval.

The two absent page indices coincide with the only pages holding edits. Page 6's LaMa patch was created at 15:32:16; page 9's paint patches at 15:34:01 and 15:34:03. These are after this batch began preparing its first uploads at 15:27:56 and before those pages would be reached. The current `cleaned_page` path checks the consent-bound appearance, which includes these visible patches. This strongly explains the pre-dispatch gaps as changed-page refusals; the running batch's final per-page report was not available during the investigation.

## Live recheck and direct transfer measurement

Fetched the affected app's logs again with an explicit `--profile yarafiq1971`; the active CLI profile was left unchanged. The refreshed log covers 15:27:48–16:24:49 and contains 111 denoise job status GETs. Of these, 57 took at least 80 seconds. Their median total duration was 88.9 seconds, while their median gateway execution duration was 3.54 seconds. The near-90-second total is consistent with the desktop request deadline; HTTP 200 is not evidence that the desktop received the complete body.

At 16:26:28, the provider Dict snapshot contained 17 denoise documents, all completed with no provider error. The corresponding batch's local snapshot contained three `run_page_complete`, twelve `unknown_remote_state`, and two `submitted_tile` records. Those local records concern the 15:27:56 batch on `mc-unbswc`, excluding an earlier cancelled batch in the same chapter. The two waiting local pages were 18 and 19; later pages had not entered the local attempt journal. The current chapter manifest confirms 24 source pages. A new three-second process sample again caught both denoise workers in `HttpGpuJob::status → job_request → read_bounded_body → Response::read`.

Read-only lookups of the two newest provider handles returned:

| Handle suffix | Provider accepted | Provider marked complete | Provider state |
| --- | --- | --- | --- |
| `f65a7c629e6e1c5efd45e6a6c32431c0` | 16:19:08 | 16:19:29 | completed, no error |
| `83cdb3e1db43865e6c2b6e813fee5be2` | 16:20:08 | 16:20:18 | completed, no error |

At 16:27:34, made one authenticated status GET for the already-completed page 12 handle, using its existing profile's runtime credential in memory. No new job, provider cancellation, or application retry was requested. Redirects and environment proxies were disabled. Received HTTP 200 and the first body bytes after 5.932 seconds. The measurement stopped at its own 40-second diagnostic deadline, having received only 69,626 body bytes; after the first byte, average throughput was about 2.0 KB/s. Byte-count checkpoints were 21,880 at 8.98 seconds, 36,858 at 24.07 seconds, and 64,136 at 33.92 seconds. This is direct evidence of a very slow/stalling HTTP response body on the affected endpoint, well below the throughput needed for a roughly 3 MB JSON result inside 90 seconds. It does not identify which network hop is responsible, and the diagnostic deadline is separate from the application's timeout.

The provider snapshot and local journal are independent, non-atomic observations of an active batch. Counts describe those observation times, not its final outcome. `run_page_complete` records a validated remote result before local restoration/file writing, so the journal alone is not proof of a saved output.

## Relevant implementation

- `src-tauri/src/inference/cloud_denoise.rs`: `PAGES_IN_FLIGHT = 2`; `run_plan` calls `denoise_one` synchronously within each worker. That call includes preparation, remote wait, restoration and file writing before the worker takes another page.
- `deploy/cloud/common/api.py`: `_route_gpu_job` attaches `_denoise_result_json` to completed status replies. That serializer expands `page_png` to base64.
- `deploy/cloud/modal/backend.py`: `ModalGpuJobs.status` fetches the existing function output again on each completed status read. Terminal documents contain metadata; the full output is fetched from Modal again for the response.
- `src-tauri/src/inference/http.rs`: `HttpGpuJob::status` reads the entire JSON body. `read_bounded_body` keeps bytes in a temporary vector and loses them when a read errors.
- `src-tauri/src/inference/gpu_jobs.rs`: status timeout is 90 seconds and the denoise schedule permits five consecutive poll failures. Those repeated full downloads explain the measured 464–501 second waits on several pages.
- `src-tauri/src/inference/http.rs`: `fetch_result_bytes_while` already retains partial bytes and resumes render PNG downloads using `Range`. Denoise does not call it.
- `src-tauri/src/inference/journal.rs`: the analysis attempt record used by denoise does not store the accepted HTTP job handle. The HTTP layer generates its own attempt ID, separate from the journal's proposal ID. A local unknown outcome therefore lacks the normal render flow's durable handle for result recovery.
- `src/lib/dialogs/DenoiseDialog.svelte`: the running state displays completion progress; the per-page failures list depends on the report returned after the batch finishes.

## Network interpretation and limits

The logs, process samples and authenticated transfer measurement establish a long response-body wait after a short gateway execution. They do not independently identify the exact congested network hop. The existing `us-east` throughput incident in `docs/findings.md` has the same pattern, so routing is a plausible contributor, not a proven complete explanation.

The measured PNGs exceed 2 MiB, and base64 increases the HTTP payload by another third. Modal's current [region-selection documentation](https://modal.com/docs/guide/region-selection) says function inputs/outputs above 2 MiB still use object storage in `us-east`. A closer routing region alone therefore cannot be assumed to fix this design. The current [data-residency documentation](https://frontend.modal.com/docs/guide/data-residency) also distinguishes spawned function payloads from synchronous regional traffic.

An earlier diagnostic `modal curl` attempt against an existing result received HTTP 401 because the gateway also requires its runtime authorization. It supplied no valid throughput measurement. The later authenticated GET above did supply one. No runtime credential was printed, written to a file, or copied to a command line, and no new benchmark job was started.

Cold starts visible in the screenshot are secondary: an eight-minute submission gap exceeds the configured 120-second idle window. Keeping the GPU warm longer would spend more on idle time while leaving the blocked downloads intact.

## Fix order

1. Separate small status metadata from image retrieval. Keep the existing inline-result route for compatibility, advertise the new operation explicitly, and have updated clients use it. Completed status should return result digest, dimensions, mode, size and a same-origin result handle/path. No arbitrary result URLs.
2. Retrieve raw PNG bytes from an authenticated result route with bounded ranges/chunks, retaining partial data and resuming after a stall. Bound individual response bodies below the large-payload threshold, including envelopes; cap total encoded/decoded size and verify the complete digest and geometry. Range support alone is insufficient if the first request still transfers the whole multi-megabyte body.
3. Persist the accepted job handle and download state before polling. A transfer failure must remain a recoverable completed/download-pending job. Retry retrieval of the same result instead of submitting the model again. Keep authority/source checks and result validation before recording or replacing page files.
4. Decouple preparation/submission from remote wait/download. Release a preparation slot when a durable accepted handle exists. Use separate limits for upload memory, pending remote jobs, and downloads so two blocked downloads do not prevent later pages from reaching the provider queue. Set limits from denoise page memory and measured link behavior; copying a 16-job render limit blindly could overload a slow link.
5. Emit each skipped/failed page and its reason immediately. Show whether work is preparing, queued/running, downloading, or saved. Re-plan a changed page with its new appearance through the existing authorization flow rather than silently uploading a different page.

Acceptance: on a deliberately interrupted result transfer, already received bytes survive, the same provider handle is used, no second GPU inference runs, and other independent pages can be submitted. A completed provider job must remain recoverable after restart. Completed status remains small. Both old and new clients interoperate with the appropriate gateway capabilities. Changed pages report their refusal immediately. Cancellation reaches queued work, downloads stop promptly, and output order/name ownership remains deterministic.

Diagnostic evidence captured under `/tmp`: `manga-denoise-yarafiq1971-20261009.log`, `manga-denoise-provider-jobs-20261009.json`, `manga-denoise-joined-timeline-20261009.json`, `manga-denoise-result-metadata-20261009.json`, and `manga-cleaner-denoise-20261009.sample.txt`. Only metadata was retained from the three result reads; PNG pixels were not saved by this investigation.

Live recheck evidence: `manga-denoise-yarafiq1971-live-20261009.log`, `manga-denoise-provider-jobs-live-20261009.json`, `manga-denoise-local-attempts-live-20261009.json`, `manga-cleaner-denoise-live-20261009.sample.txt`, and `manga-denoise-authenticated-transfer-live-20261009.json`. The transfer probe discarded body bytes after counting them and saved no image or response content.
