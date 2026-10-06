/**
 * Domain model typedefs shared across `src/lib/model`.
 *
 * This describes the in-memory shape the UI reads and Task 3's mock engine
 * produces. It is close to, but distinct from, the persisted `.mtclean` job
 * manifest: the manifest is a flat list of
 * `patches` and `regions_untouched` keyed by `source_idx`, written for one
 * job. This model groups the same information under the Projects → Chapters
 * → Pages → Regions hierarchy the UI needs (Home is a two-level hierarchy by
 * user ruling; a project owns chapters, a chapter owns pages).
 *
 * `Provenance` mirrors the persisted record's field names verbatim
 * so a patch's provenance round-trips without translation.
 *
 * No runtime code lives here - typedefs only, imported by JSDoc comments
 * elsewhere via `import('./types.js').Foo`.
 */

/**
 * A patch's reproducibility record. Every field name matches
 * the persisted record exactly.
 *
 * @typedef {Object} Provenance
 * @property {string} engine - rung id that produced this mask, e.g. 'lama'
 * @property {string} engine_version
 * @property {string|null} model_sha256
 * @property {string} execution_provider
 * @property {Record<string, unknown>} params_snapshot - thresholds, dilation, radii actually used
 * @property {string} mask_sha256
 * @property {string} source_sha256
 * @property {CloudProvenance|null} cloud
 * @property {string} created - ISO 8601 timestamp
 */

/**
 * @typedef {'beam' | 'modal'} CloudProvider
 */

/**
 * @typedef {{ type: 'local' } | { type: 'beam', profile_id: string } | { type: 'modal', profile_id: string }} ExecutionTarget
 */

/**
 * @typedef {Object} RenderRecipe
 * @property {string} recipe_id
 * @property {string} preprocessing_version
 * @property {string} model_id
 * @property {string} model_revision
 * @property {boolean} native_mask_conditioning
 * @property {{target: 'auto'|'dialogue'|'sound_effect'|'other', description: string}} [qwen_edit]
 */

/**
 * @typedef {Object} CloudProvenance
 * @property {string} provider
 * @property {string} [profile_id]
 * @property {string} [job_id]
 * @property {string} request_id
 * @property {string} [attempt_id]
 * @property {string} [recipe_id]
 * @property {string} model
 * @property {string} [model_revision]
 * @property {string} [tier]
 * @property {number|null} [cost]
 * @property {number} [duration_ms]
 */

/**
 * A legacy cloud outcome. Nothing sets one when a cloud render commits: a
 * render is recorded in `Provenance.cloud` and is not flagged for review. The
 * native side still restores a *rejection* from a job saved with an old cloud
 * review state (`src-tauri/src/library.rs#review_flags`), which is why the
 * field is on every mask it sends; `model/review.js` reads it the same way the
 * native page counts do. The old "accepted" state diagnosed nothing and is
 * migrated away on load: `accepted` alone never flags a layer.
 *
 * @typedef {Object} CloudOutcome
 * @property {boolean} accepted
 * @property {'safety-filter'|'transport-error'|'parameter-test'|'residual-test'|'structural'|null} rejectionCause
 */

/**
 * Operation intent bound to backend authorization proposals.
 *
 * @typedef {Object} OperationIntent
 * @property {'applyTool'|'createRegion'|'rerunMask'|'cleanAnyway'} action
 * @property {string} [tool]
 * @property {Record<string, unknown>} [params]
 * @property {string} [maskId]
 * @property {string} [kind]
 * @property {string} [engine]
 * @property {string} [regionId]
 */

/**
 * What the native side proposes to send for one cloud render, prepared before the consent dialog asks.
 *
 * @typedef {Object} ConsentProposal
 * @property {string} proposalId
 * @property {string} profileId
 * @property {CloudProvider} provider
 * @property {string} endpointUrl
 * @property {string} canonicalOriginFingerprint
 * @property {number} profileEpoch
 * @property {string} cropSha256
 * @property {string} hintSha256
 * @property {string} sourceHash
 * @property {string} maskHash
 * @property {string|number} regionRevision
 * @property {{x: number, y: number, w: number, h: number}} rect
 * @property {RenderRecipe} recipe
 * @property {OperationIntent} intent
 * @property {number} createdAtMs
 * @property {number} expiresAtMs
 * @property {number|null} estimatedCostUsd
 * @property {boolean} [standing] - the chapter's project already consented to this endpoint, with both statements ticked: skip the question, not the grant
 */

/**
 * Scoped authorization bounds minted upon proposal confirmation.
 *
 * @typedef {Object} GrantScope
 * @property {CloudProvider} provider
 * @property {string} profileId
 * @property {string} endpointFingerprint
 * @property {string} cropSha256
 * @property {string} maskHash
 * @property {string|number} revision
 * @property {RenderRecipe} recipe
 * @property {string} operationDigest
 */

/**
 * Backend-issued, attempt-limited authorization grant.
 *
 * @typedef {Object} Grant
 * @property {string} nonce
 * @property {GrantScope} scope
 * @property {number} issuedAtMs
 * @property {number} expiresAtMs
 * @property {number} allowedAttempts
 * @property {number} usedAttempts
 */

/**
 * Safe public connection check result (ordinary reachability, never triggers GPU work).
 *
 * @typedef {Object} CloudConnectionStatus
 * @property {boolean} ok
 * @property {string} status
 * @property {CloudProvider} provider
 * @property {string} profileId
 * @property {number} [latencyMs]
 * @property {string} [message]
 */

/**
 * Wire model metadata and provisional limits.
 *
 * @typedef {Object} CloudModelInfo
 * @property {string} supportedProtocolVersion
 * @property {string} pinnedModelId
 * @property {string} pinnedModelRevision
 * @property {string} pinnedRecipeId
 * @property {Object} limits
 * @property {[number, number]} limits.maxDimensions
 * @property {number} limits.maxMegapixels
 * @property {number} limits.maxPngBytes
 * @property {number} limits.maxMultipartBytes
 * @property {number} limits.defaultWorkerDeadlineSec
 */

/**
 * Submission response for a durable remote attempt.
 *
 * @typedef {Object} CloudAttemptSubmission
 * @property {string} attemptId
 * @property {string|null} [handle]
 * @property {'accepted'|'dispatching'|'unknown'} status
 * @property {string} [requestDigest]
 * @property {boolean} [autoRetryable]
 * @property {string} [error]
 */

/**
 * Authoritative lifecycle status for an accepted remote attempt.
 *
 * @typedef {Object} CloudAttemptStatus
 * @property {string} attemptId
 * @property {string|null} handle
 * @property {'pending'|'running'|'completed'|'failed'|'cancelled'|'cancel_requested'|'unknown'} status
 * @property {number|null} reportedCostUsd
 * @property {boolean} [acknowledged]
 * @property {number} [createdAtMs]
 * @property {number} [startedAtMs]
 * @property {number} [finishedAtMs]
 */

/**
 * Validated output crop result retrieved from local cache or remote gateway.
 *
 * @typedef {Object} CloudAttemptResult
 * @property {string} attemptId
 * @property {string} handle
 * @property {string} resultDigest
 * @property {number|null} reportedCostUsd
 * @property {number} width
 * @property {number} height
 * @property {boolean} cached
 */

/**
 * Cancellation request acknowledgement.
 *
 * @typedef {Object} CloudCancelResult
 * @property {string} handle
 * @property {'cancel_requested'|'cancelled'} status
 * @property {boolean} acknowledged
 */

/**
 * Recovery decider outcome for uncommitted or interrupted attempts.
 *
 * @typedef {Object} CloudRecoveryDecision
 * @property {'ambiguous_unknown'|'resume_polling'|'resume_cancel_polling'|'result_cached_ready'|'attachment_pending'|'stale_attachment'|'already_committed'|'terminal'} decision
 * @property {string} [attemptId]
 * @property {string|null} [handle]
 * @property {string} [resultDigest]
 * @property {string} [patchId]
 * @property {boolean} [autoRetryable]
 * @property {string} [message]
 */

/**
 * One cleaning attempt on a Region. Hand-drawn and automatic masks are the
 * same shape - there is no separate "manual mask" type.
 *
 * @typedef {Object} Mask
 * @property {string} id
 * @property {string} regionId
 * @property {number} sequence - monotonic per-region revision number; higher is newer
 * @property {'match-surround'|'reconstruct'|'solid'} fillMode
 * @property {number} elapsedMs
 * @property {boolean} fittingReconstructed - the fill failed and a model reconstructed the area
 * @property {'changed'|'unknown'|null} [dependencyReview] - earlier visible input changed after this result
 * @property {string|null} [maskReview] - a `review.reason.*` key about the mask itself: grouping evidence (`crossesBalloon`, `maskMissingUnderBox`), or `unrecognized` for a stored reason this build has no meaning for
 * @property {boolean} [generatedTextureReview] - native flag for a successful unmasked cloud inpaint on a stored detection that needs a texture check
 * @property {string|null} [maskQualityState] - a saved text-shaped result that still needs mask correction
 * @property {string|number|null} [textShapePatchRevision] - immutable text-shaped patch revision restored by history
 * @property {CloudOutcome|null} cloudOutcome - legacy; null on every mask made today
 * @property {{opacity: number, offsetX: number, offsetY: number, rotation: number, locked: boolean}} [layer] - saved presentation settings; defaults to opaque, untransformed, unlocked
 * @property {{transform: 'movable'|'fixed'|'none', lock: boolean, opacity: boolean}} [capabilities] - what the layer may do, derived natively from what produced it (`model/layers.js`)
 * @property {{x: number, y: number, w: number, h: number}} [sourceBbox] - the untransformed patch box in page percent; the region's `bbox` is the box it is drawn in
 * @property {string|null} [appearance] - native digest of everything this layer draws; null for a detection
 * @property {string|null} [layerKey] - native digest of this layer's pixels alone (`tile::layer_appearance`): its `layer` image URL is versioned by it, and an opacity change leaves it alone; null for a detection
 * @property {number|null} [order] - the layer's place in the compositing stack, lower first and ties by region id; null for a detection
 * @property {Provenance} provenance
 */

/**
 * A detected (or hand-drawn) text area on a page. `mask` is the current /
 * latest revision; earlier revisions are not kept here (never silently
 * overwritten - but retaining history is a
 * concern for the persistence layer, not this in-memory shape).
 *
 * @typedef {Object} Region
 * @property {string} id
 * @property {string} pageId
 * @property {{x: number, y: number, w: number, h: number}} bbox
 * @property {'auto'|'hand'} source
 * @property {'pending'|'cleaned'|'declined'|'gate-skipped'|'detected'|'candidate'} outcome - `detected` is a stored detection waiting to be cleaned: it carries the fitted mask the cleaner will use, and nothing on the page has changed yet (docs/detect-clean.md). `candidate` is lettering grouping held for the user to choose (no text box claimed it, or a lone island in SAM-only mode): never cleaned on its own, not a failure, not a review problem. `model/review.js#regionState` is the one reading of these
 * @property {'low-confidence'|'outside-bubble'|'not-japanese'|'not-text'|'language-skipped'|'outside-language-unverified'|null} gateSkipCause - `not-japanese` is a *confident* refusal: the gate read the script and it was not CJK
 * @property {string|null} declineReason
 * @property {string|null} [candidateReason] - set on a candidate only: the grouping reason it was held under (`review.reason.unassignedMask`, `review.reason.isolatedMask`)
 * @property {boolean|null} [candidateInsideBubble] - set on a candidate only: whether grouping found it inside a balloon; decides the engine a clean starts on (`model/review.js#heldStartsOutside`). `null` on a row stored before it was recorded
 * @property {boolean|null} [insideBubble] - set on a stored detection only: whether the run found it inside a speech bubble. Picks its mask colour (`model/masks.js#maskColorFor`). `null` on every other region
 * @property {string|null} [attention] - a cloud reason read natively from the attempt journal (`review.reason.repairNeeded`, `cloudResultNotApplied`, `cloudResultUnchecked`), on a detection or a layer; never stored
 * @property {boolean} unusuallyLarge
 * @property {Mask|null} mask - null unless outcome is `cleaned` or `detected`
 * @property {'fill'|'solid'|'lama'} [pick] - a detection's starting rung; a stored `denoise` reads as `fill` (`ladder.js#currentRung`)
 * @property {'local'|'cloud'} [detector] - where a detection was found
 */

/**
 * @typedef {Object} Page
 * @property {string} id
 * @property {string} chapterId
 * @property {number} index - 0-based position within the chapter / strip
 * @property {'unclean'|'cleaning'|'cleaned'|'skipped'|'detected'} status - `detected`: the only unfinished regions are detections waiting to be cleaned
 * @property {number} [candidateCount] - held candidates on the page, counted apart from its work (`model/status.js#pageCounts`)
 * @property {string|null} skipReason
 * @property {Region[]} regions
 */

/**
 * @typedef {Object} Chapter
 * @property {string} id
 * @property {string} projectId
 * @property {string} name
 * @property {number} order
 * @property {Page[]} pages
 */

/**
 * @typedef {Object} Project
 * @property {string} id
 * @property {string} name
 * @property {'single'|'longstrip'} mode
 * @property {'rtl'|'ltr'} readingDirection - per-project, RTL by default
 * @property {string} created
 * @property {string} appVersion
 * @property {Chapter[]} chapters
 */

export {}
