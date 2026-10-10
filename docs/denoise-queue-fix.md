# Denoise queue and result-transfer fix — 2026-10-09

Implemented after the [Yarafiq1971 latency investigation](denoise-latency-yarafiq1971-2026-10-09.md). The affected gateway returned completed pages at about 2 KB/s; each roughly 3 MB base64 status response repeatedly exceeded the desktop's 90-second deadline. Both existing denoise workers waited through the full upload, inference, result download and save, leaving later pages unsubmitted.

## Submission continues during downloads

The client uses the new path when the gateway advertises both `denoise.jobs` and `denoise.results`:

| Stage | Bound | Slot ends when |
| --- | --- | --- |
| Read, encode and upload | 2 pages | The gateway accepts the idempotent handle |
| Accepted remote work | 16 jobs | Small metadata confirms completion, or the attempt ends |
| Metadata polling | 4 workers | That job becomes ready for download or ends |
| Download, validate and save | 2 pages | The output is saved or retrieval fails |

Completed metadata waits independently of downloads. Upload/native buffers are dropped after submission; waiting jobs retain small tickets rather than full page buffers. The 24-page regression test holds both downloads blocked and requires every page to be accepted before releasing them. GPU execution concurrency is still controlled by the existing deployment.

## Completed images no longer travel inside status JSON

All routes use the configured gateway origin and its existing authentication:

- `GET /mc/denoise/v1/jobs/{handle}/metadata` returns the small job envelope and, when ready, the PNG size, SHA-256 and denoise information.
- `GET /mc/denoise/v1/jobs/{handle}/result/{index}` returns at most 32 KiB of binary PNG data. A `Range: bytes=N-` request retrieves a chunk's missing tail.
- `POST /mc/denoise/v1/jobs/{handle}/result/release` discards cached pixels after the desktop has verified and saved its output.

Each transfer request has a 30-second timeout. Received bytes append immediately to a `.part` file; a dropped response preserves them. Completed images are checked against the request digest, expected recipe/model identities, PNG dimensions and color mode, byte count and SHA-256 before attachment. Native depth, color metadata and hidden alpha samples use the existing restoration path.

The gateway stores only small metadata when polling completion. The first download caches the existing provider output in bounded chunks in the deployment's existing Dict. Later chunk reads survive a gateway restart or expiration of that provider output, as long as the cache remains available. The ready marker is published after the chunk writes. No new inference, volume or deployment resource is created by result retrieval. Modal Dict entries have an inactivity lifetime; see [Modal's Dict documentation](https://modal.com/docs/sdk/py/latest/Dict).

## Recovery and cancellation

Before POST, the client persists the exact attempt ID, derived handle, request and profile/origin binding. An unanswered submission repeats that attempt, so the provider's existing claim prevents duplicate inference. Completed metadata and partial bytes remain in the local transfer journal if download or save fails. An approved rerun of the same unchanged page, recipe and target resumes that result with the same handle. This is recovery on rerun, not automatic startup resumption. Pre-fix journals lack these tickets and cannot automatically recover their old provider handles through this new path.

An HTTP error from the status route retains the accepted ticket as an unknown outcome, rather than treating that call as failed and creating a new attempt on rerun. A typed provider failure remains a failed attempt. Authority is checked before sending and before saving. The page's consent-bound appearance is reread before restoration, so edits made while inference/download was queued prevent attachment. Cancellation stops further submissions and reaches accepted jobs, including those waiting behind pollers. Reports retain plan order.

## Validation

- Native inference suite: 345 passed, 2 ignored.
- Final denoise suite: 46 passed, including all-24 submission during blocked downloads, the 16-job bound, cancellation, edited-page refusal, partial-response tail retrieval, recovery in a new client, preservation of the accepted handle after status errors and corrupt-checkpoint rejection.
- Core denoise wire suite: 8 passed.
- Cloud suite: 391 passed, 25 environment-dependent skips (416 total). Coverage includes small metadata, lazy chunk caching, cached retrieval after provider-output expiration, ranges/authentication, cleanup and transient SDK failures.
- Frozen cloud helper rebuilt from this checkout. The desktop app bundle compiled successfully at `target/release/bundle/macos/Manga Cleaner.app`; its local ad-hoc signature passed `codesign --verify --deep --strict`. This is a local development artifact, separate from live rollout.

No new paid GPU benchmark was run; the tests establish queue independence and transfer recovery, not a measured live chapter duration.

## Rollout

Redeploy the existing `mc-unbswc` target on profile `yarafiq1971`, preserving its L40S, model selection, idle window and routing settings, then use the rebuilt desktop client. Older clients retain their existing routes. A new client talking to an older gateway retains the previous two-worker path, so both sides need the update for the fix to take effect.

The investigation verified `us-east` gateway routing (Virginia). Physical GPU placement remains unpinned; this fix does not change either setting. Source changes and local build artifacts do not update the running installation or live deployment.
