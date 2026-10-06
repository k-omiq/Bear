import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { regionMarker } from './regions.js'

/** @param {object} [overrides] */
function region(overrides = {}) {
  const id = overrides.id ?? 'r1'
  return {
    id,
    pageId: 'p1',
    bbox: { x: 10, y: 10, w: 20, h: 10 },
    source: 'auto',
    outcome: 'cleaned',
    gateSkipCause: null,
    declineReason: null,
    unusuallyLarge: false,
    detected: true,
    mask: {
      id: `${id}-m1`,
      regionId: id,
      sequence: 1,
      fillMode: 'match-surround',
      elapsedMs: 0,
      fittingReconstructed: false,
      cloudOutcome: null,
      provenance: { engine: 'fill', params_snapshot: {}, cloud: null },
    },
    ...overrides,
  }
}

describe('a detection on the canvas', () => {
  const flagged = () => region({ id: 'd', outcome: 'detected', attention: 'review.reason.repairNeeded' })

  it('stays a detection when a recovery flags it for repair: its outline, not a masked layer', () => {
    const marker = regionMarker(flagged())
    expect(marker.status).toBe('detected')
    // It carries a stored mask, and that mask is still the cleaner's input.
    expect(marker.masked).toBe(false)
    expect(regionMarker(region({ id: 'plain', outcome: 'detected' })).masked).toBe(false)
  })

  it('is named with its reason, and marked as flagged only when flags are asked for', () => {
    const marker = regionMarker(flagged())
    expect(marker.nameKey).toBe('canvas.region.nameFlagged')
    expect(marker.nameParams).toEqual({
      statusKey: 'masks.status.detected',
      titleKey: 'masks.title.detected',
      reasonKey: 'review.reason.repairNeeded',
    })
    expect(marker.badge).toBeNull()
    expect(regionMarker(flagged(), { marksVisible: true }).badge).toBe('△')
    expect(regionMarker(region({ id: 'plain', outcome: 'detected' }), { marksVisible: true }).badge).toBeNull()
  })

  it('leaves a cleaned layer masked and a flagged one marked as before', () => {
    expect(regionMarker(region()).masked).toBe(true)
    const large = region({ unusuallyLarge: true })
    expect(regionMarker(large).status).toBe('review')
    expect(regionMarker(large, { marksVisible: true }).badge).toBe('△')
  })
})

describe('a candidate on the canvas', () => {
  const candidate = () => region({ id: 'c', outcome: 'candidate', candidateReason: 'review.reason.isolatedMask', mask: null })

  it('is drawn as a candidate, named as held for the user, and never counted as a flag', () => {
    const marker = regionMarker(candidate(), { marksVisible: true })
    expect(marker.status).toBe('candidate')
    expect(marker.masked).toBe(false)
    expect(marker.badge).toBeNull()
    expect(marker.nameKey).toBe('canvas.region.name')
    expect(marker.nameParams).toEqual({ statusKey: 'review.state.candidate', titleKey: 'review.candidate.title' })
  })

  it('has an always-on dotted outline in the page mark colour, with no motion of its own', () => {
    const source = readFileSync(resolve(import.meta.dirname, 'RegionLayer.svelte'), 'utf-8')
    const rule = source.match(/\.region\.candidate \{([^}]*)\}/)?.[1] ?? ''
    expect(rule).toMatch(/outline-style:\s*dotted/)
    // The page tokens hold in both themes; the sheet is paper in each.
    expect(rule).toMatch(/outline-color:\s*var\(--page-mark\)/)
    expect(rule).not.toMatch(/animation|transition/)
  })
})
