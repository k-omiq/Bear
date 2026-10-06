/**
 * The Layers panel's row shaping: what one region says collapsed, what it
 * shows expanded, and what it offers to do about itself. Pure, and every
 * decision it makes is deferred to `src/lib/model` - `review.js` for whether a
 * region needs review and why, `masks.js` for ordering, provenance and the
 * action sets, `ladder.js` for engine names. Nothing here re-derives any of
 * them; this module only turns them into rows.
 *
 * Text arrives as i18n keys, never as strings: a row descriptor is data, and
 * the component is the only thing that calls `t()`.
 */

import {
  detectedActions,
  engineChoiceLabel,
  fillModeLabel,
  isDetected,
  maskEngine,
  orderMasks,
  provenanceFacts,
  reRunnable,
  reviewEntryActions,
} from '../model/masks.js'
import { candidateReason, needsReview, regionState, reviewList, reviewReason } from '../model/review.js'
import { rungLabel } from '../model/ladder.js'
import { knownModelName } from '../model/model-names.js'

/**
 * @typedef {Object} Fact
 * @property {string} key - i18n key for the label column
 * @property {string} [valueKey] - i18n key for the value, when the value is words
 * @property {Object} [params] - params for `valueKey`
 * @property {string} [value] - the value verbatim, when it is a proper noun (a version, a request id)
 */

/**
 * @typedef {Object} MaskRow
 * @property {string} id - the region's id
 * @property {string|null} maskId
 * @property {'applied'|'review'|'declined'|'unexamined'|'detected'|'candidate'} status
 * @property {string} glyph - `▪` applied, `△` needs review, `!` declined, `○` not examined, `◇` detected, `◌` candidate
 * @property {string} statusKey - the same status in words, for assistive tech
 * @property {string} titleKey - engine name, or what the row is when there is no mask
 * @property {string|null} modelId - pinned cloud or recorded local FLUX model, when known
 * @property {Array<{key: string, params?: Object}>} sub - the sub-line, in parts
 * @property {string|null} reasonKey - why it needs review, if it does
 * @property {string|null} candidateReasonKey - why a candidate was held, on a candidate row only; never a review reason
 * @property {boolean} deletable - always; every row can be removed, warnings included
 * @property {string|null} engine - what produced the mask, for the row's engine picker: a rung, or `cloud` for a mask the cloud rendered
 * @property {boolean} reRunnable - whether the row may offer Try again and an engine picker
 * @property {Fact[]} facts
 * @property {import('../model/masks.js').MaskAction[]} actions
 */

/**
 * One row status per region state (`model/review.js#regionState`), under the
 * row's own long-standing names: `applied` is `cleaned`, `review` is
 * `needsReview`, `declined` is `failed`, `unexamined` is `pending`.
 *
 * `unexamined` is the one a three-state classification had to lie about: a
 * region with no mask that nothing has yet declined, held back or flagged has
 * not been *examined*, and calling it `applied` made the canvas announce "No
 * mask - Applied" on every uncleaned page. `candidate` is the one a review
 * row had to lie about: a held suggestion is not a problem, and listing it as
 * "declined, every engine failed" (which is what an unknown held row used to
 * read as) sent the user looking for a fault that was never there.
 */
const STATUS = Object.freeze({
  applied: { glyph: '▪', statusKey: 'masks.status.applied' },
  review: { glyph: '△', statusKey: 'masks.status.needsReview' },
  declined: { glyph: '!', statusKey: 'masks.status.declined' },
  unexamined: { glyph: '○', statusKey: 'masks.status.unexamined' },
  // Found and stored, not cleaned: the Pages list marks its page the same way.
  detected: { glyph: '◇', statusKey: 'masks.status.detected' },
  // Held for a choice: a dotted outline, because nothing has been decided.
  candidate: { glyph: '◌', statusKey: 'review.state.candidate' },
})

/**
 * The needs-review mark on its own, for the canvas: a detection flagged for
 * repair keeps the detected status and glyph, and the canvas still marks it
 * as flagged when the overlay or the review filter asks for flags.
 */
export const REVIEW_GLYPH = STATUS.review.glyph

/** @type {Readonly<Record<import('../model/review.js').RegionState, keyof typeof STATUS>>} */
const ROW_STATUS = Object.freeze({
  cleaned: 'applied',
  needsReview: 'review',
  failed: 'declined',
  pending: 'unexamined',
  detected: 'detected',
  candidate: 'candidate',
})

/**
 * The rows the panel lists, in the order it lists them.
 *
 * Unfiltered: all regions in scope - masked regions newest first (`orderMasks`),
 * then detections, then candidates, then the other maskless regions (gate
 * warnings, declined, unexamined). Candidates sit together, right under the
 * work still to clean, so the sound effects and artwork grouping held back are
 * found in one place. Filtered: exactly `reviewList` over the same regions, in
 * *its* order, so the list is a contiguous slice of the chapter review set the
 * bottom-right pill's arrows step through - and a candidate, which is not a
 * problem, is not in it. Re-sorting the filtered list by revision, as the
 * design prototype does, would give the two surfaces two different orderings
 * of one set.
 *
 * @param {import('../api/backend.js').ApiRegion[]} regions - the panel's scope
 * @param {{ filtered?: boolean }} [options]
 * @returns {MaskRow[]}
 */
export function maskRows(regions, options = {}) {
  if (options.filtered) return reviewList(regions).map((entry) => maskRow(entry.region, true))

  // Detections carry a mask too, but it is the cleaner's input and has no
  // revision to order by: they follow the layers, in the order they came.
  const masked = regions.filter((region) => region.mask && !isDetected(region))
  const byMaskId = new Map(masked.map((region) => [region.mask.id, region]))
  const orderedMasked = orderMasks(masked.map((region) => region.mask)).map((mask) => byMaskId.get(mask.id))
  const detected = regions.filter(isDetected)
  const candidates = regions.filter(isCandidate)
  const unmasked = regions.filter((region) => !region.mask && !isDetected(region) && !isCandidate(region))
  return [...orderedMasked, ...detected, ...candidates, ...unmasked].map((region) => maskRow(region, false))
}

/** @param {import('../api/backend.js').ApiRegion} region */
function isCandidate(region) {
  return regionState(region) === 'candidate'
}

/**
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {boolean} filtered - whether the row is being shown by the review filter
 * @returns {MaskRow}
 */
export function maskRow(region, filtered) {
  if (isDetected(region)) return detectedRow(region)
  if (isCandidate(region)) return candidateRow(region)
  const mask = region.mask ?? null
  const reasonKey = reviewReason(region)
  const status = ROW_STATUS[regionState(region)]

  return {
    id: region.id,
    maskId: mask ? mask.id : null,
    status,
    glyph: STATUS[status].glyph,
    statusKey: STATUS[status].statusKey,
    titleKey: titleKeyFor(region, mask),
    modelId: mask?.provenance?.cloud?.model ??
      (maskEngine(mask) === 'flux' ? mask?.provenance?.params_snapshot?.flux_model ?? null : null),
    sub: (filtered || !mask) && reasonKey ? [{ key: reasonKey }] : subLine(region, mask),
    reasonKey,
    candidateReasonKey: null,
    // Every row, warnings included. A warning with no way off the list is a
    // list that only grows: a region the user has looked at and decided to
    // leave alone has to be dismissable, and dismissing it is the same act as
    // deleting a mask - the region goes, and undo brings it back.
    deletable: true,
    // `cloud` for a patch the cloud rendered, which the native side records
    // as the FLUX rung with a cloud record: the row says where it ran.
    engine: maskEngine(mask),
    reRunnable: reRunnable(mask),
    facts: factsFor(region, mask, reasonKey),
    // Only the two an unmasked review entry offers. A mask's own four -
    // stronger, simpler, cycle fill, reopen - are gone: they were four buttons
    // asking the user to reason about an engine ladder, and the row now says
    // the same thing in one picker and one Try again.
    actions: mask ? [] : reviewEntryActions(region),
  }
}

/**
 * A detected region's row: named for what it is, with where it was found and
 * the engine it will start on, and the two ways to clean it. No engine picker
 * and no Try again - there is no result yet to swap or repeat - and Delete,
 * like every row, through the trash icon.
 *
 * The cloud action is always listed here; the row drops it while the cloud
 * cannot be used, because this module does not read the cloud's state.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {MaskRow}
 */
function detectedRow(region) {
  const pick = typeof region.pick === 'string' ? region.pick : null
  // A detection a committed cloud result went missing from: still a detection
  // to clean, and flagged for the repair, which leads the row.
  const reasonKey = reviewReason(region)
  const sub = reasonKey ? [{ key: reasonKey }] : [{ key: 'masks.sub.detected' }]
  if (pick) sub.push({ key: 'masks.sub.startsWith', params: { engineKey: engineChoiceLabel(pick === 'solid' ? 'fill' : pick) } })
  /** @type {Fact[]} */
  const facts = [
    { key: 'masks.provenance.applied', valueKey: 'masks.value.nothingYet' },
    { key: 'masks.provenance.foundOn', valueKey: region.detector === 'cloud' ? 'masks.value.foundCloud' : 'masks.value.foundLocal' },
  ]
  if (pick) {
    facts.push({ key: 'masks.provenance.startsWith', valueKey: pick === 'solid' ? 'tools.option.modeSolid' : engineChoiceLabel(pick) })
  }
  if (reasonKey) facts.push({ key: 'masks.provenance.flagged', valueKey: reasonKey })
  // Still `detected`, flag or not: the status is what the row, the canvas and
  // the cloud action read to know this is work waiting to be cleaned, and a
  // `review` status made a flagged detection a masked layer there. The flag
  // travels as `reasonKey`, which the review filter and counts read.
  return {
    id: region.id,
    maskId: region.mask?.id ?? null,
    status: 'detected',
    glyph: STATUS.detected.glyph,
    statusKey: STATUS.detected.statusKey,
    titleKey: 'masks.title.detected',
    modelId: null,
    sub,
    reasonKey,
    candidateReasonKey: null,
    deletable: true,
    engine: null,
    reRunnable: false,
    facts,
    actions: detectedActions({ cloud: true }),
  }
}

/**
 * A candidate's row: what grouping held back and why, and the two things to do
 * about it - clean it, or look at it on the page. Dismissing it is Delete, as
 * on every row. It carries no review reason, because it is not a problem, and
 * so the review filter, the page's review count and ⌃ ⌄ all pass it by.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @returns {MaskRow}
 */
function candidateRow(region) {
  const reason = /** @type {string} */ (candidateReason(region))
  return {
    id: region.id,
    maskId: null,
    status: 'candidate',
    glyph: STATUS.candidate.glyph,
    statusKey: STATUS.candidate.statusKey,
    titleKey: 'review.candidate.title',
    modelId: null,
    sub: [{ key: reason }],
    reasonKey: null,
    candidateReasonKey: reason,
    deletable: true,
    engine: null,
    reRunnable: false,
    facts: [
      { key: 'masks.provenance.applied', valueKey: 'masks.value.nothingApplied' },
      { key: 'review.candidate.heldBecause', valueKey: reason },
    ],
    actions: [
      { id: 'cleanAnyway', labelKey: 'review.candidate.clean', hintKey: 'review.candidate.cleanHint' },
      { id: 'showOnPage', labelKey: 'masks.action.showOnPage' },
    ],
  }
}

/**
 * A mask is named by the engine that produced it, Cloud for one the cloud
 * rendered; a region with no mask is named by what happened to it instead,
 * since "nothing" is exactly the case the user cannot see on the page.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {import('../model/types.js').Mask|null} mask
 * @returns {string}
 */
function titleKeyFor(region, mask) {
  if (mask) return rungLabel(maskEngine(mask) ?? '')
  if (region.outcome === 'declined') return 'masks.title.declined'
  if (region.outcome === 'gate-skipped') return 'masks.title.gateSkipped'
  return 'masks.title.noMask'
}

/**
 * Fill mode · cost · hand - the parts, not the sentence. A hand-drawn mask is
 * marked because that is the one thing about it a reader cannot infer;
 * everything else about it is identical to an automatic one.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {import('../model/types.js').Mask|null} mask
 * @returns {Array<{key: string, params?: Object}>}
 */
function subLine(region, mask) {
  if (!mask) return [{ key: 'masks.sub.noMask' }]
  const parts = []
  if (mask.maskQualityState) parts.push({ key: 'review.reason.maskNeedsCorrection' })
  if (mask.provenance.cloud) {
    if (typeof mask.provenance.cloud.cost === 'number' && Number.isFinite(mask.provenance.cloud.cost)) {
      parts.push({ key: 'masks.value.cloudCost', params: { cost: mask.provenance.cloud.cost } })
    }
  }
  return parts
}

/**
 * The expanded row. For a mask that is `provenanceFacts` - engine, model
 * version, fill mode, elapsed, and for a cloud region the cost and request id
 * - with its raw values turned into display keys, plus where the mask came
 * from. For a region with no mask it is what was applied (nothing) and whether
 * the detector ever saw it. Either way, a flagged region ends with why.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {import('../model/types.js').Mask|null} mask
 * @param {string|null} reasonKey
 * @returns {Fact[]}
 */
function factsFor(region, mask, reasonKey) {
  const facts = mask
    ? provenanceFacts(mask).map(displayFact)
    : [
        { key: 'masks.provenance.applied', valueKey: 'masks.value.nothingApplied' },
        {
          key: 'masks.provenance.detected',
          valueKey: region.detected ? 'masks.value.detectedYes' : 'masks.value.detectedNo',
        },
      ]
  if (reasonKey) facts.push({ key: 'masks.provenance.flagged', valueKey: reasonKey })
  if (reasonKey === 'review.reason.checkGeneratedTexture') {
    facts.push({ key: 'review.fact.detail', valueKey: 'review.detail.generatedTexture' })
  }
  // A failure says what failed. `review.reason.declined` is the kind; the
  // decliner's own `decline.reason.*` key is the cause, and it is the part a
  // reader acts on - an engine that was missing is not a quality verdict.
  if (!mask && region.outcome === 'declined' && region.declineReason?.startsWith('decline.reason.')) {
    facts.push({ key: 'review.fact.cause', valueKey: region.declineReason })
  }
  return facts
}

/**
 * `masks.js` returns raw values - a rung id, a fill mode, a millisecond count
 * - and leaves the formatting to whoever displays them. This is that.
 *
 * @param {import('../model/masks.js').ProvenanceFact} fact
 * @returns {Fact}
 */
function displayFact(fact) {
  switch (fact.key) {
    case 'masks.provenance.engine':
      return { key: fact.key, valueKey: rungLabel(String(fact.value)) }
    case 'masks.provenance.fillMode':
      return { key: fact.key, valueKey: fillModeLabel(String(fact.value)) }
    case 'masks.provenance.elapsed':
      return { key: fact.key, valueKey: 'masks.value.elapsed', params: { ms: fact.value } }
    case 'masks.provenance.cloudCost':
      if (typeof fact.value === 'number' && Number.isFinite(fact.value)) {
        return { key: fact.key, valueKey: 'masks.value.cloudCost', params: { cost: fact.value } }
      }
      // No cost came back with the render. Saying so beats a guessed one.
      return { key: fact.key, valueKey: 'masks.value.cloudCostUnknown' }
    case 'masks.provenance.model':
      return { key: fact.key, value: knownModelName(fact.value) ?? String(fact.value ?? '') }
    default:
      // A model version and a cloud request id are proper nouns; translating
      // them would be translating an identifier.
      return { key: fact.key, value: String(fact.value ?? '') }
  }
}

/**
 * What an action's tooltip says.
 *
 * One branch now that the four ladder actions are gone. It stays a function
 * rather than becoming a template literal at the call site because an action
 * whose hint wants a parameter is one edit away, and because the catalogue test
 * enumerates the keys through it.
 *
 * @param {string} actionId
 * @returns {{ key: string, params?: Object }}
 */
export function actionHint(actionId) {
  return { key: `masks.hint.${actionId}` }
}

/**
 * How many of a scope's regions need review. The filter button's count, and
 * the same predicate the filtered list itself uses.
 *
 * @param {import('../api/backend.js').ApiRegion[]} regions
 * @returns {number}
 */
export function flaggedCount(regions) {
  return regions.filter(needsReview).length
}

/**
 * How many of a scope's regions are candidates awaiting a choice. Counted
 * apart from `flaggedCount`, by the same state the rows are drawn from.
 *
 * @param {import('../api/backend.js').ApiRegion[]} regions
 * @returns {number}
 */
export function candidateCount(regions) {
  return regions.filter(isCandidate).length
}
