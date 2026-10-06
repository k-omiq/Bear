import { describe, expect, it } from 'vitest'
import { pageCounts, pageMark, recountPage } from './status.js'

/** @returns {import('./types.js').Page} */
function page(overrides = {}) {
  return { id: 'p1', chapterId: 'c1', index: 0, status: 'unclean', skipReason: null, regions: [], ...overrides }
}

/** @returns {import('./types.js').Region} */
function declinedRegion(id) {
  return {
    id,
    pageId: 'p1',
    bbox: { x: 0, y: 0, w: 1, h: 1 },
    source: 'auto',
    outcome: 'declined',
    gateSkipCause: null,
    declineReason: 'quality',
    unusuallyLarge: false,
    mask: null,
  }
}

describe('pageMark', () => {
  it('· not yet cleaned', () => {
    expect(pageMark(page({ status: 'unclean' }))).toEqual({
      glyph: '·', tone: 'muted', count: 0, titleKey: 'pages.status.unclean',
    })
  })

  it('● cleaning now', () => {
    expect(pageMark(page({ status: 'cleaning' }))).toEqual({
      glyph: '●', tone: 'active', count: 0, titleKey: 'pages.status.cleaning',
    })
  })

  it('✓ cleaned, nothing to review', () => {
    expect(pageMark(page({ status: 'cleaned', regions: [] }))).toEqual({
      glyph: '✓', tone: 'normal', count: 0, titleKey: 'pages.status.cleaned',
    })
  })

  it('✓ n cleaned, n regions need review', () => {
    const regions = [declinedRegion('a'), declinedRegion('b')]
    expect(pageMark(page({ status: 'cleaned', regions }))).toEqual({
      glyph: '✓', tone: 'warn', count: 2, titleKey: 'pages.status.cleanedNeedsReview',
    })
  })

  it('! skipped', () => {
    expect(pageMark(page({ status: 'skipped', skipReason: 'corrupt header' }))).toEqual({
      glyph: '!', tone: 'warn', count: 0, titleKey: 'pages.status.skipped',
    })
  })
})

describe('a page with detections waiting', () => {
  const detection = { ...declinedRegion('d'), outcome: 'detected', declineReason: null, detected: true, mask: { id: 'd-m1' } }

  it('◇ is marked detected', () => {
    expect(pageMark(page({ status: 'detected', regions: [detection] }))).toMatchObject({
      glyph: '◇', tone: 'detected', titleKey: 'pages.status.detected',
    })
  })

  it('counts a detection as found, not done, and not as needing review', () => {
    expect(pageCounts(/** @type {any} */ (page({ status: 'detected', regions: [detection] })))).toEqual({ total: 1, done: 0, review: 0, candidates: 0 })
  })

  it('carries the count of a detection flagged for repair', () => {
    const repair = { ...detection, id: 'r', attention: 'review.reason.repairNeeded' }
    const regions = [detection, repair]
    expect(pageCounts(/** @type {any} */ (page({ status: 'detected', regions })))).toEqual({ total: 2, done: 0, review: 1, candidates: 0 })
    expect(pageMark(page({ status: 'detected', regions }))).toEqual({
      glyph: '◇', tone: 'warn', count: 1, titleKey: 'pages.status.detected',
    })
  })
})

describe('candidates are counted apart', () => {
  const candidate = { ...declinedRegion('c'), outcome: 'candidate', declineReason: null, candidateReason: 'review.reason.unassignedMask' }
  const cleaned = { ...declinedRegion('k'), outcome: 'cleaned', declineReason: null, mask: { id: 'k-m1' } }

  it('a resident page counts its candidates, not as work or review', () => {
    const counts = pageCounts(/** @type {any} */ (page({ status: 'cleaned', regions: [cleaned, candidate] })))
    expect(counts).toEqual({ total: 1, done: 1, review: 0, candidates: 1 })
    expect(pageMark(page({ status: 'cleaned', regions: [cleaned, candidate] }))).toMatchObject({ glyph: '✓', tone: 'normal', count: 0 })
  })

  it('a header reads the same numbers off the native counts, and recounting agrees', () => {
    const resident = /** @type {any} */ (page({ status: 'cleaned', regions: [cleaned, candidate, declinedRegion('d')] }))
    recountPage(resident)
    expect([resident.regionCount, resident.doneCount, resident.reviewCount, resident.candidateCount]).toEqual([3, 1, 1, 1])
    const header = /** @type {any} */ (page({ status: 'cleaned', resident: false, regions: [],
      regionCount: 3, doneCount: 1, reviewCount: 1, candidateCount: 1 }))
    expect(pageCounts(header)).toEqual(pageCounts(resident))
  })

  it('marks a page containing only held candidates as waiting, including from a header', () => {
    const resident = /** @type {any} */ (page({ status: 'cleaned', regions: [candidate] }))
    const header = /** @type {any} */ (page({ status: 'cleaned', resident: false,
      regionCount: 1, doneCount: 0, reviewCount: 0, candidateCount: 1 }))
    for (const current of [resident, header]) {
      expect(pageMark(current)).toEqual({ glyph: '◇', tone: 'detected', count: 0, titleKey: 'pages.status.held' })
    }
  })
})
