/**
 * What state a region is in, why it is flagged when it is, and the filtered,
 * steppable list the Masks panel's review filter and the bottom bar's
 * ⌃ ⌄ issue navigation are built on.
 *
 * ## Six states, kept apart
 *
 * `regionState` answers one of these for every region, and nothing else in
 * the interface re-derives them:
 *
 *  - `candidate` - lettering no text box claimed (or, SAM-only, a lone island),
 *    held by grouping so that artwork is never erased on a guess. Awaiting the
 *    user's choice. **Not a failure and not a problem**: it has a reason
 *    (`candidateReason`), but no review reason, and it is in no review count.
 *  - `detected` - found and stored, waiting to be cleaned. A detection whose
 *    cloud work needs a look (`attention`) is `needsReview` instead; its Layers
 *    row still reads as a detection, with the reason beside it.
 *  - `cleaned` - a layer with nothing flagged.
 *  - `failed` - processing failed and nothing was applied: every engine
 *    declined it.
 *  - `needsReview` - flagged, with an evidence-backed reason: a cleaned layer
 *    with a diagnosed problem, or a region held back with one (the script
 *    gate, a text box with no lettering under it, a mask that needs
 *    correction).
 *  - `pending` - nothing has examined it yet.
 *
 * `failed` and `needsReview` are the flagged states: both carry a
 * `reviewReason` and both are in the review list, the page's review count and
 * the ⌃ ⌄ navigation. The other four never are.
 *
 * ## Review causes, checked in this order
 *
 *  0. its cloud work needs a look, read natively from the attempt journal
 *     (`region.attention`): a committed result went missing, one was never
 *     applied, or one could not be checked. The one cause a detection can
 *     carry, and it can sit on a layer too.
 *  1. an earlier edit changed what the layer read
 *  2. fitting failed and a model reconstructed the area
 *  3. the region is unusually large for the page
 *  4. the text-shaped mask needs correction
 *  5. grouping evidence against the mask (it crosses its balloon, or a text
 *     box had no lettering under it), or a stored reason this build does not
 *     recognise
 *  6. a successful unmasked cloud inpaint needs a texture check
 *  7. the app declined
 *  8. the script gate skipped it - low confidence, or text outside a bubble
 *  9. a legacy cloud rejection on the mask
 *
 * This is `src-tauri/src/library.rs#review_reason` clause for clause, and the
 * two are pinned by the parity test there: the native side counts every page
 * header and the chapter review index with it, and this counts the regions
 * the interface is holding.
 *
 * **Using the cloud alone is not a cause.** The texture check requires the
 * native API's specific successful unmasked inpaint flag. The legacy "cloud
 * request accepted" flag, which old jobs carry
 * on every accepted render, diagnosed nothing: the native side migrates it
 * away on load and `cloudOutcome.accepted` flags nothing here. A cloud
 * *rejection* is a processing failure and stays flagged.
 *
 * A region can technically match more than one cause (e.g. declined *and*
 * unusually large); `reviewReason` reports the first match in the order
 * above, since that is also review priority - a fit failure is more
 * actionable than a size heuristic.
 */

/**
 * The gate's three held-back causes. Three, not two: the backend's
 * `Verdict::NotJapanese` is a *confident* refusal - the gate read the script and
 * it was Latin - and the two-value union this used to be folded it into
 * `low-confidence`, reporting the opposite of what happened.
 */
const GATE_SKIP_KEYS = {
  'low-confidence': 'review.reason.gateSkippedLowConfidence',
  'outside-bubble': 'review.reason.gateSkippedOutsideBubble',
  'not-japanese': 'review.reason.gateSkippedNotJapanese',
  'not-text': 'review.reason.gateSkippedNotText',
  'language-skipped': 'review.reason.languageSkipped',
  'outside-language-unverified': 'review.reason.outsideLanguageUnverified',
}

const CLOUD_REJECTION_KEYS = {
  'safety-filter': 'review.reason.cloudRejectedSafetyFilter',
  'transport-error': 'review.reason.cloudRejectedTransportError',
  'parameter-test': 'review.reason.cloudRejectedParameterTest',
  'residual-test': 'review.reason.cloudRejectedResidualTest',
  structural: 'review.reason.cloudRejectedStructural',
}

/**
 * Grouping evidence against a mask (`cleaner_core::text_groups::ReviewReason`).
 * A declined region stored under one of these was held for what it says about
 * the mask, not because an engine failed on it.
 */
const MASK_EVIDENCE_KEYS = new Set(['review.reason.crossesBalloon', 'review.reason.maskMissingUnderBox'])

/**
 * The grouping reasons a candidate is held under. Anything else on a
 * candidate is named as the first of them rather than left blank.
 */
const CANDIDATE_KEYS = new Set(['review.reason.unassignedMask', 'review.reason.isolatedMask'])

/**
 * Which of the plan's kinds of reason each review cause is. A flagged region
 * always shows its own specific reason; the category is what decides whether
 * it is a failure (`processing`) or something to look at, and it is what the
 * catalogue test checks every cause against, so a new cause cannot arrive
 * without being placed.
 *
 * @type {Readonly<Record<string, 'input'|'fitting'|'size'|'mask'|'texture'|'processing'|'gate'|'unknown'>>}
 */
export const REASON_CATEGORY = Object.freeze({
  'review.reason.repairNeeded': 'input',
  'review.reason.cloudResultNotApplied': 'processing',
  'review.reason.cloudResultUnchecked': 'unknown',
  'review.reason.inputChanged': 'input',
  'review.reason.inputUnknown': 'input',
  'review.reason.fittingReconstructed': 'fitting',
  'review.reason.unusuallyLarge': 'size',
  'review.reason.maskNeedsCorrection': 'mask',
  'review.reason.checkGeneratedTexture': 'texture',
  'review.reason.crossesBalloon': 'mask',
  'review.reason.maskMissingUnderBox': 'mask',
  'review.reason.unrecognized': 'unknown',
  'review.reason.declined': 'processing',
  'review.reason.cloudRejectedSafetyFilter': 'processing',
  'review.reason.cloudRejectedTransportError': 'processing',
  'review.reason.cloudRejectedParameterTest': 'processing',
  'review.reason.cloudRejectedResidualTest': 'processing',
  'review.reason.cloudRejectedStructural': 'processing',
  'review.reason.gateSkippedLowConfidence': 'gate',
  'review.reason.gateSkippedOutsideBubble': 'gate',
  'review.reason.gateSkippedNotJapanese': 'gate',
  'review.reason.gateSkippedNotText': 'gate',
  'review.reason.languageSkipped': 'gate',
  'review.reason.outsideLanguageUnverified': 'gate',
})

/**
 * @param {import('./types.js').Region} region
 * @returns {string|null} the i18n key for the review cause, or null if the region needs no review
 */
export function reviewReason(region) {
  // The one flag a detection can carry, on a layer too: its cloud work needs
  // a look (`region.attention`, read natively from the attempt journal).
  if (region.attention) return region.attention
  // A detection waits to be cleaned and a candidate waits to be chosen;
  // nothing has been done to either that could want a second look yet
  // (docs/detect-clean.md). A detection's fitted mask is the cleaner's input,
  // not a result, so none of the causes below read it.
  if (region.outcome === 'detected' || region.outcome === 'candidate') return null
  const mask = region.mask
  if (mask?.dependencyReview) return mask.dependencyReview === 'changed' ? 'review.reason.inputChanged' : 'review.reason.inputUnknown'
  if (mask?.fittingReconstructed) return 'review.reason.fittingReconstructed'
  if (region.unusuallyLarge) return 'review.reason.unusuallyLarge'
  if (mask?.maskQualityState || region.declineReason === 'review.reason.maskNeedsCorrection') {
    return 'review.reason.maskNeedsCorrection'
  }
  if (mask?.maskReview) return mask.maskReview
  if (mask?.generatedTextureReview) return 'review.reason.checkGeneratedTexture'
  if (region.outcome === 'declined') {
    return MASK_EVIDENCE_KEYS.has(region.declineReason ?? '') ? /** @type {string} */ (region.declineReason) : 'review.reason.declined'
  }
  if (region.outcome === 'gate-skipped') {
    // An unrecognised cause still flags the region. Returning null here would
    // drop it out of review entirely, which is the one outcome worse than
    // naming the cause imprecisely.
    return GATE_SKIP_KEYS[region.gateSkipCause] ?? 'review.reason.gateSkippedLowConfidence'
  }
  const rejection = mask?.cloudOutcome?.rejectionCause
  return rejection ? (CLOUD_REJECTION_KEYS[rejection] ?? null) : null
}

/**
 * @param {import('./types.js').Region} region
 * @returns {boolean}
 */
export function needsReview(region) {
  return reviewReason(region) !== null
}

/**
 * Why a candidate was held, or null for anything that is not a candidate.
 *
 * @param {import('./types.js').Region} region
 * @returns {string|null}
 */
export function candidateReason(region) {
  if (region.outcome !== 'candidate') return null
  const key = region.candidateReason ?? ''
  return CANDIDATE_KEYS.has(key) ? key : 'review.reason.unassignedMask'
}

/**
 * Whether a clean of a held-back region the user starts without naming an
 * engine begins on the outside-bubble pick rather than the in-bubble one.
 *
 * **One rule with the native side** (`src-tauri/src/region.rs`
 * `untouched_fallback_pick`): text the gate held outside a bubble, and a
 * candidate grouping did not find inside a balloon - usually a sound effect
 * over artwork, where a flat fill would paint over the drawing - start
 * outside. A candidate stored before its balloon was recorded has no answer
 * and starts outside too, as the safer of the two.
 *
 * @param {import('./types.js').Region} region
 * @returns {boolean}
 */
export function heldStartsOutside(region) {
  if (region.outcome === 'candidate') return region.candidateInsideBubble !== true
  return region.gateSkipCause === 'outside-bubble'
}

/**
 * @typedef {'candidate'|'detected'|'cleaned'|'failed'|'needsReview'|'pending'} RegionState
 */

/**
 * The one state a region is in. See the module header for what each means.
 *
 * `failed` is a processing failure with nothing applied. A processing reason
 * on a region that *does* hold a layer (a legacy cloud rejection over a local
 * result) is a layer to look at, so it is `needsReview`.
 *
 * @param {import('./types.js').Region} region
 * @returns {RegionState}
 */
export function regionState(region) {
  if (region.outcome === 'candidate') return 'candidate'
  const reason = reviewReason(region)
  if (reason) return !region.mask && REASON_CATEGORY[reason] === 'processing' ? 'failed' : 'needsReview'
  if (region.outcome === 'detected') return 'detected'
  return region.mask ? 'cleaned' : 'pending'
}

/**
 * @typedef {Object} ReviewEntry
 * @property {string} id - the region's id; stable key for list rendering
 * @property {string} pageId
 * @property {string} reasonKey
 * @property {import('./types.js').Region} region
 */

/**
 * Builds the ordered, ready-to-render review list for a scope of regions.
 * The caller decides what the scope is - a page's regions, a whole
 * chapter's, or (longstrip) just the regions in the current viewport
 * - this only filters and shapes them.
 *
 * @param {import('./types.js').Region[]} scope
 * @returns {ReviewEntry[]}
 */
export function reviewList(scope) {
  const entries = []
  for (const region of scope) {
    const reasonKey = reviewReason(region)
    if (reasonKey) entries.push({ id: region.id, pageId: region.pageId, reasonKey, region })
  }
  return entries
}

/**
 * Steps to the next or previous issue in a review list, wrapping around at
 * either end. Used by the bottom bar's ⌃ ⌄ controls. Returns null for an
 * empty list; if `currentId` is not in the list, starts from the first
 * entry (stepping next) or the last (stepping prev).
 *
 * @param {ReviewEntry[]} list
 * @param {string|null} currentId
 * @param {'next'|'prev'} direction
 * @returns {ReviewEntry|null}
 */
export function stepIssue(list, currentId, direction) {
  if (list.length === 0) return null
  const index = list.findIndex((entry) => entry.id === currentId)
  if (index === -1) return direction === 'next' ? list[0] : list[list.length - 1]
  const step = direction === 'next' ? 1 : -1
  return list[(index + step + list.length) % list.length]
}
