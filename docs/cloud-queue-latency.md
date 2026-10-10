# Cloud job queue and latency analysis — 2026-10-09

The application was limiting submission ahead of the provider queue. GPU concurrency and submission concurrency are separate: Modal's render worker executes up to two pipeline copies, and its analysis worker accepts three concurrent inputs, but additional accepted calls can wait in the provider queue.

## Causes and changes

| Layer | Previous behavior | Change |
| --- | --- | --- |
| Bulk cleaning client | Three workers each held their slot through crop preparation, upload, GPU polling, download and commit. Later independent regions stayed on the computer. | Up to 16 remote jobs, with only three preparations/uploads at once. A durable accepted handle releases the preparation slot. Upload buffers are dropped before polling. |
| Cleaning dependencies | A region reads earlier patches on the same page or across a long-strip join. | Keep the dependency until commit. Uploading those regions early would render obsolete inputs and fail attachment. |
| Selection detection UI | Per-page gesture tails, a singleton pending run claim and the batch run state prevented independent detections from being submitted together. | Each rectangle gets an independent run, status and cancellation. Other detection rectangles can be drawn while those jobs run. |
| Selection detection backend | The chapter reservation refused a second run on the same chapter. | Area-only detection runs can coexist; cloud area jobs have a separate 16-job bound. Every cloud prefetch starts before a shared three-slot gate bounds ordinary/area walks and their local CTD/OCR processing. Local jobs retain the three-run memory bound. Batch runs remain exclusive on their chapters. Results merge under the manifest lock from a fresh read. |
| Selection detection upload | Each selection prefetched all tiles of its page. | Only tiles whose cores meet the detector's context window are encoded and sent. The window includes the selection and is at least 1024 pixels per axis, limited to the page. Standard tile coordinates, halos and stitching are retained. Full-page consent remains the upper bound; the operation sends a subset. |
| Detection GPU lifetime | The desktop sent an idle-only analysis Stop after the prefetch producer finished its last page, bypassing the configured provider idle window. A short selection therefore discarded its warm models immediately. Denoise completion could release the same analysis GPU. | Let the shared analysis GPU stay available for its configured idle window (120 seconds by default, configurable from 60 to 600). Detection and denoise completion no longer send an automatic Stop; the explicit Stop action still cancels and releases it. |
| Gateway submission | One exclusive admission lock covered preflight, durable writes and provider spawn for both render and analysis. | Concurrent submissions share admission; GPU Stop closes admission for its role and waits for admitted calls to be recorded before cancellation. |
| Queued detection cancellation | Modal kept only the newest 32 analysis calls, even though submitted jobs outlive their HTTP requests. | Retain every live queued call until it settles or expires, so Stop reaches older jobs too. |
| Cleaning HTTP transport | The batch had an authenticated client for metadata, while each render built another client. | Reuse the batch's authenticated client and connection pool, retaining profile epoch and current-selection checks before submission. |

Individual cleaning from a Layers row already submits directly after consent; there was no frontend two-job cap on that path. The two visible running renders correspond to GPU pipeline capacity. The bulk clean scheduler did prevent a useful queue from forming.

Selection jobs appear separately in the jobs list and can be recovered when reopening a chapter. `list_jobs` includes `area: true` and `areaPageIndex` for them, including before `page-started`. The editor reloads a completed area's page rather than applying an older concurrent page snapshot. Ordinary edits remain blocked while detection holds that page. Interrupted areas save their own captured policy atomically with the page checkpoint; another completed area cannot overwrite or erase it. The chapter's single resume slot represents the latest interrupted area.

## Where time still goes

The path is: readiness and consent metadata → local input preparation/PNG encoding → HTTP upload and gateway validation → durable provider dispatch → provider queue and container startup → model loading/warmup → inference → polling, download and attachment.

These changes remove completion-based submission waits and redundant whole-page selection work. They do not remove model loading or guarantee that every accepted job starts immediately on a GPU.

Existing measurements in [findings.md](findings.md) are useful evidence, but are not benchmarks of this change:

- The 2026-10-01 detection experiment measured about 2.3 seconds of gateway overhead per tile, compared with roughly 0.05 seconds for RT and 0.7 seconds for SAM. Submit/poll overhead and the 0.5/1/2-second polling backoff can exceed warm inference time. Page batching already shipped and reduced the six-page run from 166 seconds to 60–73 seconds. This change keeps that batching.
- Historical L4 4B cleaning measurements were approximately 52–55 seconds cold versus 7 seconds warm. The current render worker loads and warms its pipelines on cold starts. Its GPU memory snapshot remains disabled: an earlier live trial rebuilt snapshots repeatedly and worsened cold starts to 171–186 seconds.
- Readiness checks still verify the installed weights and reload the weights volume under its existing lock. Removing or caching these checks without a verified invalidation contract could accept jobs against changed or missing models.
- `/warmup` reports `on_demand`; it does not start or load a GPU. Readiness alone cannot be interpreted as a warm worker.
- Modal analysis status currently becomes `running` after provider spawn, before actual GPU execution. That label includes provider queueing and cold startup, so it does not measure GPU start latency.

## Validation and rollout

Regression coverage verifies submission before any earlier render completes, the three-preparation bound, dependency ordering, cancellation of queued work, concurrent detection acceptance, fresh-manifest result merging, page busy tracking, multiple-job recovery, and late page snapshots.

Validation passed: 2,320 frontend tests, followed by 87 focused tests after the final UI race fixes; 412 cloud tests with 25 environment-dependent skips, followed by 107 focused gateway tests; and 319 native tests across run orchestration, analysis, cleaning and service suites. The final production frontend build and `git diff --check` also passed.

The follow-up lifecycle/local-detection change passed all 2,331 frontend tests and the production frontend build; 78 cloud GPU/job tests; 37 native analysis, 15 denoise, 187 run, 20 workflow, 12 placement and 6 core SAM tests. Artifact-dependent tests remain ignored in normal suites. The installed native CTD + Ogkalu Full + SAM pipeline also passed explicit CPU and Apple WebGPU full-page/selection smoke runs with saved/reopened masks and evidence; see [local detection smoke evidence](local-detection-smoke-2026-10-09.md).

No live cloud deployment or new paid GPU benchmark was performed. The desktop changes require rebuilding the application; gateway changes require updating the deployed gateway. For a live comparison, record timestamps for action issued, upload started, accepted handle, container heartbeat `starting`, heartbeat `running`, inference begin/end and result attached. Run the same queued rectangles and cleaning regions once cold and once warm. Provider spawn/status timestamps alone cannot distinguish cold loading from actual inference.

The analysis GPU now honors its configured idle window, so another selection during that window can reuse loaded models. This fix needs the rebuilt desktop client; its automatic Stop request was the cause. Keeping a GPU resident beyond that window can reduce cold starts between short sessions but bills more idle time. Measure the rebuilt/redeployed queue behavior before extending idle windows or retrying snapshots. Render cleanup's existing automatic idle release is unchanged.
