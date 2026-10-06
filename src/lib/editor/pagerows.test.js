import { describe, expect, it } from 'vitest'
import { ROW_HEIGHT, VIRTUAL_THRESHOLD, padPosition, pageRow, rowWindow } from './pagerows.js'

/**
 * @param {Partial<Object>} region
 */
function region(overrides = {}) {
  return {
    id: 'r1',
    pageId: 'p1',
    bbox: { x: 0, y: 0, w: 1, h: 1 },
    source: 'auto',
    outcome: 'cleaned',
    gateSkipCause: null,
    declineReason: null,
    unusuallyLarge: false,
    detected: true,
    mask: mask(),
    ...overrides,
  }
}

function mask(overrides = {}) {
  return {
    id: 'r1-m1',
    regionId: 'r1',
    sequence: 1,
    fillMode: 'match-surround',
    elapsedMs: 12,
    fittingReconstructed: false,
    cloudOutcome: null,
    provenance: { engine: 'fill', engine_version: '1.0', cloud: null },
    ...overrides,
  }
}

function page(overrides = {}) {
  return { id: 'p1', number: 3, status: 'cleaned', skipReason: null, regions: [], ...overrides }
}

describe('pageRow', () => {
  it('counts only finished regions as cleaned', () => {
    const row = pageRow(
      page({
        regions: [
          region({ id: 'a' }),
          region({ id: 'b', unusuallyLarge: true }),
          region({ id: 'c', mask: null, outcome: 'pending' }),
        ],
      }),
      { index: 2 },
    )
    expect(row.cleaned).toBe(1)
    expect(row.total).toBe(3)
    expect(row.percent).toBe(33)
    expect(row.tone).toBe('review')
  })

  it('counts an off-window page from its header, not from its empty region list', () => {
    // Everything outside the resident window arrives as a header.
    // Counting `regions` here drew `0 / 0` and an empty track for every page
    // but the three in hand.
    const row = pageRow(
      page({
        regions: [],
        resident: false,
        regionCount: 8,
        doneCount: 6,
        reviewCount: 2,
      }),
      { index: 4 },
    )
    expect(row.total).toBe(8)
    expect(row.cleaned).toBe(6)
    expect(row.percent).toBe(75)
    expect(row.tone).toBe('review')
    expect(row.mark).toEqual({
      glyph: '\u2713', tone: 'warn', count: 2, titleKey: 'pages.status.cleanedNeedsReview',
    })
  })

  it('prefers the live regions of a page the window is holding', () => {
    // A resident page has just been edited; its header counts are one edit
    // behind and the regions are the copy that moved.
    const row = pageRow(
      page({
        regions: [region({ id: 'a' })],
        resident: true,
        regionCount: 9,
        doneCount: 9,
        reviewCount: 9,
      }),
      { index: 0 },
    )
    expect(row.total).toBe(1)
    expect(row.cleaned).toBe(1)
    expect(row.mark.count).toBe(0)
  })

  it('marks a cleaned page with no regions as complete', () => {
    const row = pageRow(page({ regions: [] }), { index: 0 })
    expect(row.total).toBe(0)
    expect(row.percent).toBe(100)
    expect(row.mark.glyph).toBe('✓')
    expect(row.tone).toBe('normal')
  })

  it('leaves an untouched page empty rather than full', () => {
    const row = pageRow(page({ status: 'unclean', regions: [] }), { index: 0 })
    expect(row.percent).toBe(0)
    expect(row.mark.glyph).toBe('·')
  })

  it('carries a skipped page reason and tone', () => {
    const row = pageRow(
      page({ status: 'skipped', skipReason: 'input.skipReason.partialDecode' }),
      { index: 8 },
    )
    expect(row.mark.glyph).toBe('!')
    expect(row.tone).toBe('warn')
    expect(row.skipReasonKey).toBe('input.skipReason.partialDecode')
  })

  it('reports the review count on a cleaned page through the mark', () => {
    const row = pageRow(page({ regions: [region({ unusuallyLarge: true })] }), { index: 0 })
    expect(row.mark.glyph).toBe('✓')
    expect(row.mark.count).toBe(1)
    expect(row.mark.titleKey).toBe('pages.status.cleanedNeedsReview')
  })

  it('labels positions rather than pages in longstrip', () => {
    expect(pageRow(page(), { index: 0 }).labelKey).toBe('pages.label.page')
    expect(pageRow(page(), { index: 0, longstrip: true }).labelKey).toBe('pages.label.position')
  })

  it('zero-pads to two digits and no further', () => {
    expect(padPosition(7)).toBe('07')
    expect(padPosition(12)).toBe('12')
    expect(padPosition(140)).toBe('140')
  })
})

describe('rowWindow', () => {
  it('renders every row below the threshold', () => {
    const win = rowWindow({ count: 20, scrollTop: 0, viewportHeight: 200 })
    expect(win).toEqual({
      virtual: false,
      start: 0,
      end: 20,
      extra: null,
      extraBefore: false,
      padTop: 0,
      padGap: 0,
      padBottom: 0,
    })
  })

  it('renders every row at exactly the threshold', () => {
    expect(rowWindow({ count: VIRTUAL_THRESHOLD, viewportHeight: 200 }).virtual).toBe(false)
  })

  it('windows a long list around the scroll position', () => {
    const win = rowWindow({ count: 400, scrollTop: 25 * 100, viewportHeight: 250, overscan: 2 })
    expect(win.virtual).toBe(true)
    expect(win.start).toBe(98)
    expect(win.end).toBe(113)
    expect(win.padTop).toBe(98 * ROW_HEIGHT)
    expect(win.padBottom).toBe((400 - 113) * ROW_HEIGHT)
  })

  it('keeps the spacers and the rows adding up to the full list height', () => {
    const win = rowWindow({ count: 400, scrollTop: 900, viewportHeight: 300 })
    expect(height(win)).toBe(400 * ROW_HEIGHT)
  })

  it('clamps at the top of the list', () => {
    const win = rowWindow({ count: 400, scrollTop: 0, viewportHeight: 300 })
    expect(win.start).toBe(0)
    expect(win.padTop).toBe(0)
  })

  it('clamps at the bottom of the list', () => {
    const win = rowWindow({ count: 400, scrollTop: 400 * ROW_HEIGHT, viewportHeight: 300 })
    expect(win.end).toBe(400)
    expect(win.padBottom).toBe(0)
  })

  it('survives a nonsense scroll position', () => {
    const win = rowWindow({ count: 400, scrollTop: Number.NaN, viewportHeight: undefined })
    expect(win.start).toBe(0)
    expect(win.end).toBeGreaterThan(0)
  })

  it('renders nothing at all for a nonsense count', () => {
    const win = rowWindow({ count: Number.NaN, viewportHeight: 250 })
    expect(win).toMatchObject({ virtual: false, start: 0, end: 0, padTop: 0, padBottom: 0 })
  })

  /*
   * The point of the virtual window: a focused row far from the scroll
   * position costs one row, not every row between the two. Widening the band
   * to reach it mounts the lot, which is what this list exists to avoid.
   */
  it('keeps the focused row mounted without mounting everything in between', () => {
    const win = rowWindow({ count: 400, scrollTop: 25 * 300, viewportHeight: 250, include: 4 })
    expect(win.extra).toBe(4)
    expect(win.extraBefore).toBe(true)
    expect(win.start).toBeGreaterThan(280)
    expect(win.end - win.start).toBeLessThan(30)
    expect(win.padTop).toBe(4 * ROW_HEIGHT)
    expect(height(win)).toBe(400 * ROW_HEIGHT)
  })

  it('renders a focused row below the band after it, spacers still adding up', () => {
    const win = rowWindow({ count: 400, scrollTop: 0, viewportHeight: 250, include: 380 })
    expect(win.extra).toBe(380)
    expect(win.extraBefore).toBe(false)
    expect(win.end).toBeLessThan(380)
    expect(win.padBottom).toBe((400 - 381) * ROW_HEIGHT)
    expect(height(win)).toBe(400 * ROW_HEIGHT)
  })

  it('keeps a focused row that is already in the band in the band', () => {
    const win = rowWindow({ count: 400, scrollTop: 0, viewportHeight: 250, include: 3 })
    expect(win.extra).toBeNull()
    expect(win.padGap).toBe(0)
    expect(height(win)).toBe(400 * ROW_HEIGHT)
  })

  it('truncates a fractional include rather than letting the heights drift', () => {
    const win = rowWindow({ count: 400, scrollTop: 25 * 300, viewportHeight: 250, include: 4.5 })
    expect(win.extra).toBe(4)
    expect(height(win)).toBe(400 * ROW_HEIGHT)
  })

  it('ignores an include that is not a row', () => {
    const win = rowWindow({ count: 400, scrollTop: 0, viewportHeight: 250, include: -1 })
    expect(win.start).toBe(0)
    expect(win.extra).toBeNull()
    expect(rowWindow({ count: 400, viewportHeight: 250, include: 999 }).extra).toBeNull()
    expect(rowWindow({ count: 400, viewportHeight: 250, include: 999 }).end).toBeLessThan(400)
  })
})

/**
 * Every px the list stands for: the three spacers, the band, and the one row
 * rendered outside it. Must equal the whole list's height, or the scroller
 * lies about how far it can scroll.
 *
 * @param {import('./pagerows.js').RowWindow} win
 */
function height(win) {
  const rendered = (win.end - win.start + (win.extra === null ? 0 : 1)) * ROW_HEIGHT
  return win.padTop + win.padGap + rendered + win.padBottom
}
