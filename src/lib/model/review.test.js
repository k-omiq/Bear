import { describe, expect, it } from 'vitest'
import { REASON_CATEGORY, candidateReason, heldStartsOutside, needsReview, regionState, reviewList, reviewReason, stepIssue } from './review.js'
import { hasKey } from '../i18n/index.js'

/** @returns {import('./types.js').Region} */
function region(overrides = {}) {
  return {
    id: 'r1',
    pageId: 'p1',
    bbox: { x: 0, y: 0, w: 10, h: 10 },
    source: 'auto',
    outcome: 'cleaned',
    gateSkipCause: null,
    declineReason: null,
    unusuallyLarge: false,
    mask: null,
    ...overrides,
  }
}

/** @returns {import('./types.js').Mask} */
function mask(overrides = {}) {
  return {
    id: 'm1',
    regionId: 'r1',
    sequence: 1,
    fillMode: 'reconstruct',
    elapsedMs: 1200,
    fittingReconstructed: false,
    cloudOutcome: null,
    provenance: {
      engine: 'lama',
      engine_version: '1.0',
      model_sha256: null,
      execution_provider: 'cpu',
      params_snapshot: {},
      mask_sha256: 'x',
      source_sha256: 'y',
      cloud: null,
      created: '2026-08-11T00:00:00Z',
    },
    ...overrides,
  }
}

describe('needsReview / reviewReason', () => {
  it('a cleaned region with no flags needs no review', () => {
    const r = region({ mask: mask() })
    expect(needsReview(r)).toBe(false)
    expect(reviewReason(r)).toBeNull()
  })

  it('fitting failed and a model reconstructed the area', () => {
    const r = region({ mask: mask({ fittingReconstructed: true }) })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe('review.reason.fittingReconstructed')
  })

  it('the region is unusually large for the page', () => {
    const r = region({ mask: mask(), unusuallyLarge: true })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe('review.reason.unusuallyLarge')
  })

  it('the app declined', () => {
    const r = region({ outcome: 'declined', declineReason: 'quality', mask: null })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe('review.reason.declined')
  })

  it('the script gate skipped it on low confidence', () => {
    const r = region({ outcome: 'gate-skipped', gateSkipCause: 'low-confidence', mask: null })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe('review.reason.gateSkippedLowConfidence')
  })

  it('the script gate skipped text outside a bubble', () => {
    const r = region({ outcome: 'gate-skipped', gateSkipCause: 'outside-bubble', mask: null })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe('review.reason.gateSkippedOutsideBubble')
  })

  // The backend's third gate outcome. Confidently not CJK is not the same as
  // "not confident", and the two-value union this used to be reported it as
  // the latter.
  it('the script gate read the script and it was not Japanese', () => {
    const r = region({ outcome: 'gate-skipped', gateSkipCause: 'not-japanese', mask: null })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe('review.reason.gateSkippedNotJapanese')
  })

  it.each([
    ['language-skipped', 'review.reason.languageSkipped'],
    ['outside-language-unverified', 'review.reason.outsideLanguageUnverified'],
  ])('keeps %s candidates held for review', (cause, expectedKey) => {
    const r = region({ outcome: 'gate-skipped', gateSkipCause: cause, mask: null })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe(expectedKey)
  })

  it('keeps a saved text-shaped mask correction state in review', () => {
    const r = region({ mask: mask({ maskQualityState: 'emptySupport' }) })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe('review.reason.maskNeedsCorrection')
  })

  it('flags only the native marked cloud texture result and preserves stronger reasons', () => {
    const textured = region({ mask: mask({ generatedTextureReview: true }) })
    expect(reviewReason(textured)).toBe('review.reason.checkGeneratedTexture')
    expect(regionState(textured)).toBe('needsReview')
    expect(reviewList([textured]).map((entry) => entry.reasonKey)).toEqual(['review.reason.checkGeneratedTexture'])
    expect(reviewReason(region({ mask: mask({ generatedTextureReview: false }) }))).toBeNull()
    expect(reviewReason(region({ outcome: 'detected', mask: mask({ generatedTextureReview: true }) }))).toBeNull()
    expect(reviewReason(region({ mask: mask({ generatedTextureReview: true, maskReview: 'review.reason.crossesBalloon' }) })))
      .toBe('review.reason.crossesBalloon')
    expect(reviewReason(region({ mask: mask({ generatedTextureReview: true, fittingReconstructed: true }) })))
      .toBe('review.reason.fittingReconstructed')
  })

  it('keeps an unpatched declined candidate that needs correction visible', () => {
    const r = region({ outcome: 'declined', declineReason: 'review.reason.maskNeedsCorrection', mask: null })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe('review.reason.maskNeedsCorrection')
  })

  it('an unrecognised gate cause still flags the region rather than hiding it', () => {
    const r = region({ outcome: 'gate-skipped', gateSkipCause: null, mask: null })
    expect(needsReview(r)).toBe(true)
  })

  it.each([
    ['safety-filter', 'review.reason.cloudRejectedSafetyFilter'],
    ['transport-error', 'review.reason.cloudRejectedTransportError'],
    ['parameter-test', 'review.reason.cloudRejectedParameterTest'],
    ['residual-test', 'review.reason.cloudRejectedResidualTest'],
    ['structural', 'review.reason.cloudRejectedStructural'],
  ])('a cloud request rejected by %s', (cause, expectedKey) => {
    const r = region({ mask: mask({ cloudOutcome: { accepted: false, rejectionCause: cause } }) })
    expect(needsReview(r)).toBe(true)
    expect(reviewReason(r)).toBe(expectedKey)
  })

  // The legacy accepted flag diagnosed nothing: a successful cloud layer is a
  // cleaned layer, not a problem for having used the cloud.
  it('a cloud request accepted is not a reason', () => {
    const r = region({ mask: mask({ cloudOutcome: { accepted: true, rejectionCause: null } }) })
    expect(needsReview(r)).toBe(false)
    expect(reviewReason(r)).toBeNull()
    expect(regionState(r)).toBe('cleaned')
  })
})

describe('reviewList', () => {
  it('keeps only regions needing review, in scope order', () => {
    const clean = region({ id: 'a', mask: mask() })
    const flagged = region({ id: 'b', mask: mask({ fittingReconstructed: true }) })
    const alsoFlagged = region({ id: 'c', outcome: 'declined', mask: null })
    expect(reviewList([clean, flagged, alsoFlagged]).map((e) => e.id)).toEqual(['b', 'c'])
  })
})

describe('stepIssue', () => {
  const list = reviewList([
    region({ id: 'a', outcome: 'declined' }),
    region({ id: 'b', outcome: 'declined' }),
    region({ id: 'c', outcome: 'declined' }),
  ])

  it('steps forward and wraps past the end', () => {
    expect(stepIssue(list, 'a', 'next').id).toBe('b')
    expect(stepIssue(list, 'c', 'next').id).toBe('a')
  })

  it('steps backward and wraps past the start', () => {
    expect(stepIssue(list, 'c', 'prev').id).toBe('b')
    expect(stepIssue(list, 'a', 'prev').id).toBe('c')
  })

  it('an empty list is a no-op', () => {
    expect(stepIssue([], 'anything', 'next')).toBeNull()
    expect(stepIssue([], null, 'prev')).toBeNull()
  })
})

describe('a stored detection', () => {
  it('is waiting to be cleaned, which is not needing review', () => {
    const detection = region({ outcome: 'detected', detected: true, mask: mask(), unusuallyLarge: true })
    expect(reviewReason(detection)).toBeNull()
    expect(needsReview(detection)).toBe(false)
  })

  it('whose committed cloud result went missing is flagged for repair, and stays a detection to clean', () => {
    const detection = region({ outcome: 'detected', detected: true, mask: mask(), attention: 'review.reason.repairNeeded' })
    expect(reviewReason(detection)).toBe('review.reason.repairNeeded')
    expect(regionState(detection)).toBe('needsReview')
    expect(REASON_CATEGORY['review.reason.repairNeeded']).toBe('input')
    expect(reviewList([detection]).map((entry) => entry.reasonKey)).toEqual(['review.reason.repairNeeded'])
  })

  it('a layer the journal flags carries the reason too, and a successful one does not', () => {
    const lost = region({ mask: mask(), attention: 'review.reason.repairNeeded' })
    expect(regionState(lost)).toBe('needsReview')
    const unapplied = region({ mask: mask(), attention: 'review.reason.cloudResultNotApplied' })
    expect(reviewReason(unapplied)).toBe('review.reason.cloudResultNotApplied')
    expect(regionState(region({ mask: mask(), attention: null }))).toBe('cleaned')
  })
})

describe('regionState - candidate, detected, cleaned, failed and needs review stay apart', () => {
  const candidate = region({ id: 'cand', outcome: 'candidate', candidateReason: 'review.reason.unassignedMask', mask: null })
  const detected = region({ id: 'det', outcome: 'detected', mask: mask() })
  const cleaned = region({ id: 'ok', mask: mask() })
  const failed = region({ id: 'fail', outcome: 'declined', declineReason: 'decline.reason.qualityMetric', mask: null })
  const flagged = region({ id: 'flag', mask: mask({ dependencyReview: 'changed' }) })

  it('gives each its own state', () => {
    expect([candidate, detected, cleaned, failed, flagged].map(regionState))
      .toEqual(['candidate', 'detected', 'cleaned', 'failed', 'needsReview'])
    expect(regionState(region({ outcome: 'pending', mask: null }))).toBe('pending')
  })

  it('a candidate carries its evidence reason and is never a review problem', () => {
    expect(candidateReason(candidate)).toBe('review.reason.unassignedMask')
    expect(reviewReason(candidate)).toBeNull()
    expect(reviewList([candidate, detected, cleaned])).toEqual([])
    // An unknown held key still names a real candidate reason.
    expect(candidateReason(region({ outcome: 'candidate', candidateReason: 'something' }))).toBe('review.reason.unassignedMask')
    expect(candidateReason(cleaned)).toBeNull()
  })

  it('only failed and needs-review regions are in the review list', () => {
    expect(reviewList([candidate, detected, cleaned, failed, flagged]).map((entry) => entry.id)).toEqual(['fail', 'flag'])
  })

  it('a successful clean does not become a warning, local or cloud', () => {
    const cloud = region({ mask: mask({ provenance: { ...mask().provenance, engine: 'cloud', cloud: { provider: 'modal', model: 'flux', request_id: 'r' } } }) })
    expect(regionState(cloud)).toBe('cleaned')
    expect(regionState(region({ mask: mask({ provenance: { ...mask().provenance, engine: 'lama' } }) }))).toBe('cleaned')
  })

  it('a processing reason over a layer that exists is a layer to look at, not a failure', () => {
    const rejected = region({ mask: mask({ cloudOutcome: { accepted: false, rejectionCause: 'structural' } }) })
    expect(regionState(rejected)).toBe('needsReview')
  })
})

describe('grouping evidence on masks', () => {
  it.each([
    ['review.reason.crossesBalloon'],
    ['review.reason.maskMissingUnderBox'],
    ['review.reason.unrecognized'],
  ])('a cleaned layer stored with %s is flagged under it', (key) => {
    const r = region({ mask: mask({ maskReview: key }) })
    expect(reviewReason(r)).toBe(key)
    expect(regionState(r)).toBe('needsReview')
  })

  it('a held text box with no lettering is flagged for its mask, not as a failure', () => {
    const r = region({ outcome: 'declined', declineReason: 'review.reason.maskMissingUnderBox', mask: null })
    expect(reviewReason(r)).toBe('review.reason.maskMissingUnderBox')
    expect(regionState(r)).toBe('needsReview')
  })
})

describe('REASON_CATEGORY', () => {
  it('places every cause reviewReason can return, and each has words', () => {
    for (const key of Object.keys(REASON_CATEGORY)) expect(hasKey(key), key).toBe(true)
    const causes = [
      region({ mask: mask({ dependencyReview: 'changed' }) }),
      region({ mask: mask({ dependencyReview: 'unknown' }) }),
      region({ mask: mask({ fittingReconstructed: true }) }),
      region({ mask: mask(), unusuallyLarge: true }),
      region({ mask: mask({ maskQualityState: 'x' }) }),
      region({ mask: mask({ generatedTextureReview: true }) }),
      region({ outcome: 'declined', mask: null }),
      region({ outcome: 'gate-skipped', gateSkipCause: 'outside-bubble', mask: null }),
      region({ mask: mask({ cloudOutcome: { accepted: false, rejectionCause: 'safety-filter' } }) }),
      region({ outcome: 'detected', mask: mask(), attention: 'review.reason.repairNeeded' }),
      region({ mask: mask(), attention: 'review.reason.cloudResultNotApplied' }),
      region({ mask: mask(), attention: 'review.reason.cloudResultUnchecked' }),
    ]
    for (const r of causes) expect(REASON_CATEGORY[/** @type {string} */ (reviewReason(r))]).toBeTruthy()
  })
})

describe('heldStartsOutside - one rule with region.rs#untouched_fallback_pick', () => {
  it('starts a candidate where its balloon says, and outside when it has none or predates the record', () => {
    const candidate = (inside) => region({ outcome: 'candidate', candidateReason: 'review.reason.unassignedMask', candidateInsideBubble: inside })
    expect(heldStartsOutside(candidate(true))).toBe(false)
    expect(heldStartsOutside(candidate(false))).toBe(true)
    expect(heldStartsOutside(candidate(null))).toBe(true)
    expect(heldStartsOutside(candidate(undefined))).toBe(true)
  })

  it('starts gate-held text outside only when it was outside a bubble', () => {
    const gated = (cause) => region({ outcome: 'gate-skipped', gateSkipCause: cause })
    expect(heldStartsOutside(gated('outside-bubble'))).toBe(true)
    expect(heldStartsOutside(gated('low-confidence'))).toBe(false)
    expect(heldStartsOutside(gated('not-japanese'))).toBe(false)
  })
})
