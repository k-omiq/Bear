# Qwen lettering guidance and review

Merged from the isolated `qwen-targeted-prompts` worktree into `codex/cloud-integration`, preserving the image-retention, adjacent-page preload, and clone-source marker fixes. The deployed worker has not been updated by this merge. No new GPU tests were performed during integration.

Qwen Clean asks for the lettering type and an optional description such as “Big black letters beside the hand.” It never asks the user to write a model prompt. Single-region defaults use the region's known type; batch Clean offers Automatic for mixed lettering. Cancelling this dialog creates no consent proposal and sends no crop. The dialogue/sound-effect choices preserve the other type of text. The worker adds a coarse location from the hint and fixed instructions to reconstruct hidden background and preserve artwork, outlines, tone, and surrounding details. The description supplements these instructions. Accepted guidance is saved in patch provenance and prefilled when cleaning that region again.

Guidance is a bounded `qwen_edit` field on the requested recipe. Native consent, grant equality, batch plan hashes, Python/Rust request digests, and the durable attempt record bind it. Unicode descriptions are supported; controls and descriptions over 500 characters are rejected. A shared fixture verifies both implementations hash the same request. The pipeline receives a prompt per call; shared global/pipeline settings are not mutated between users.

Recipe `mc-qwen-image-edit-2511-v4` pins seed 1, eight steps, CFG 1, the matching Lightning 8-step V1.0 adapter, and FukidashiErase, both active at weight 1. The model and adapter revisions stay pinned. The existing provisioner's content marker changes with the adapter filename, so setup must seed the matching adapter. Older Qwen deployments are refused before descriptions could be ignored. FLUX recipes and sampling remain unchanged.

The worker keeps the three-seed retry check. Exhausting all three below edit ratio 2 returns `edit_not_applied`, rather than returning the best failed image. A passing ratio is only evidence of pixel change: it does not prove the lettering disappeared or that artwork survived. No automatic OCR quality claim is made.

The client caches each successful Qwen result and shows Before/After of the actual tone-aligned blend through the prepared write mask. Only Use result records acceptance and permits attachment. Discard is terminal and retains the original page; recovery cannot attach a discarded candidate. Unreviewed cached results require review after restart. The callback holds neither the project nor attempt lock while waiting, times out after ten minutes, and respects cancellation/cloud disable. Attachment revalidates the source, revision, crop, hint and underlay after review.

Single-region review allows a revised description and Try again; it requires a changed description, acquires a fresh grant, and is another paid request. Batch and recovered candidates show Use/Discard; a discarded batch region remains available for a separate single-region retry. The batch estimate uses a linear eight-step estimate from the earlier four-step measurement and includes two additional renders in its high bound. That estimate is not a new GPU measurement.

Rollout requires rebuilding the client and updating/reseeding the Qwen deployment with the integrated cloud code. Merging the source does not replace the installed app or alter the production volume. The prior best diagnostic result and settings in `/Users/caved/comic-translate/qwen-d74-best-*` remain untouched; the new generic prompt builder has not been GPU-benchmarked against that hand-crafted prompt.

## Integration review

Three bug hunters reviewed the integrated frontend, native inference/recovery, and worker/provisioner paths. The follow-up fixes cover:

- Batch render requests use the exact recipe and guidance bound to each region grant; guided/default Qwen and FLUX have regression coverage.
- Recovered jobs offer Use/Discard, without a Retry action that has no new-grant runner.
- Recovery reconciles result files written before a crash before deciding whether human review is needed, and never attaches without acceptance.
- Overlapping recovery calls have one review owner; a second call cannot replace the waiter or discard its cached result.
- Saved Automatic guidance is visibly selected when reopening a single-region prompt.
- Prompt/review drafts survive another modal appearing.
- Native review-close and terminal-attempt events dismiss expired or cancelled reviews, including recovered cached results, without losing requested retry guidance.
- Changed Qwen recipes/adapters redeploy Beam's seed, worker, gateway, and endpoint before reseeding; unchanged resumes keep their existing behavior and persistent resources.

The image retention, adjacent-page preload, and clone-source visibility fixes remain in place. Review and tests use local fixtures and fake gateways; this integration did not deploy cloud resources or perform paid GPU runs.

### Integration validation

- Frontend: 123 test files, 2,218 passing tests; production frontend build passes.
- Native inference: 326 passing tests, 2 intentionally ignored platform probes, no failures.
- Core Qwen: 2 passing tests, including the Python/Rust Unicode request-digest fixture and masked-preview preservation.
- Worker/provisioner: 194 passing tests with NumPy and Pillow available; no skips.
- Frozen cloud helper: rebuilt from the integrated sources; self-check and offline Modal/Beam failure smoke tests pass.
- macOS release: rebuilt and installed at `/Applications/Manga Cleaner.app`; strict signature verification passes, and the installed executable matches the built executable with Qwen guidance/review commands present.

The integrated client still requires the compatible `mc-qwen-image-edit-2511-v4` deployment before it offers Qwen guidance. Updating a local app does not redeploy the saved cloud endpoint.
