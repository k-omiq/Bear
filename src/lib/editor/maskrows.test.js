import { describe, expect, it } from 'vitest'
import { candidateCount, flaggedCount, maskRow, maskRows } from './maskrows.js'
import { reviewList } from '../model/review.js'

function mask(overrides = {}) {
  return {
    id: `${overrides.regionId ?? 'r1'}-m1`,
    regionId: 'r1',
    sequence: 1,
    fillMode: 'match-surround',
    elapsedMs: 40,
    fittingReconstructed: false,
    cloudOutcome: null,
    provenance: {
      engine: 'fill',
      engine_version: 'planar-1.4',
      model_sha256: null,
      execution_provider: 'cpu',
      params_snapshot: {},
      mask_sha256: 'a',
      source_sha256: 'b',
      cloud: null,
      created: '2026-01-01T00:00:00.000Z',
    },
    ...overrides,
  }
}

function region(overrides = {}) {
  const id = overrides.id ?? 'r1'
  return {
    id,
    pageId: 'p1',
    bbox: { x: 0, y: 0, w: 1, h: 1 },
    source: 'auto',
    outcome: 'cleaned',
    gateSkipCause: null,
    declineReason: null,
    unusuallyLarge: false,
    detected: true,
    mask: mask({ id: `${id}-m1`, regionId: id }),
    ...overrides,
  }
}

describe('maskRows - unfiltered', () => {
  it('lists masked regions newest first and includes unmasked ones', () => {
    const rows = maskRows([
      region({ id: 'a', mask: mask({ id: 'a-m1', regionId: 'a', sequence: 1 }) }),
      region({ id: 'b', mask: null, outcome: 'gate-skipped', gateSkipCause: 'low-confidence' }),
      region({ id: 'c', mask: mask({ id: 'c-m1', regionId: 'c', sequence: 9 }) }),
    ])
    expect(rows.map((row) => row.id)).toEqual(['c', 'a', 'b'])
    expect(rows[2].status).toBe('review')
    expect(rows[2].glyph).toBe('△')
    expect(rows[2].titleKey).toBe('masks.title.gateSkipped')
    expect(rows[2].sub).toEqual([{ key: 'review.reason.gateSkippedLowConfidence' }])
    expect(rows[2].deletable).toBe(true)
  })

  it('names a row by the engine that produced it', () => {
    const [row] = maskRows([
      region({ mask: mask({ provenance: { ...mask().provenance, engine: 'lama' } }) }),
    ])
    expect(row.titleKey).toBe('ladder.rung.lama')
    expect(row.status).toBe('applied')
    expect(row.glyph).toBe('▪')
    expect(row.deletable).toBe(true)
  })

  // Denoise fill is gone; a patch saved by it is a fill now.
  it('names a patch saved as the retired denoise rung Fill', () => {
    const [row] = maskRows([
      region({ mask: mask({ provenance: { ...mask().provenance, engine: 'denoise' } }) }),
    ])
    expect(row.titleKey).toBe('ladder.rung.fill')
    expect(row.engine).toBe('fill')
  })

  it('keeps cloud cost on the sub-line without redundant layer details', () => {
    const cloud = mask({
      fillMode: 'reconstruct',
      provenance: {
        ...mask().provenance,
        engine: 'cloud',
        cloud: { provider: 'x', model: 'y', request_id: 'req-1', tier: 'a', cost: 0.014 },
      },
    })
    const [row] = maskRows([region({ source: 'hand', mask: cloud })])
    expect(row.sub).toEqual([
      { key: 'masks.value.cloudCost', params: { cost: 0.014 } },
    ])
  })

  it('offers a mask no expandable actions - its controls are on the row itself', () => {
    const [row] = maskRows([region()])
    expect(row.actions).toEqual([])
    expect(row.deletable).toBe(true)
    expect(row.reRunnable).toBe(true)
    expect(row.engine).toBe('fill')
  })

  it('offers Try again and the engine picker on a cloud mask', () => {
    const cloud = mask({
      provenance: {
        ...mask().provenance,
        engine: 'cloud',
        cloud: { provider: 'x', model: 'y', request_id: 'r', tier: 'a', cost: 0.01 },
      },
    })
    const [row] = maskRows([region({ mask: cloud })])
    expect(row.engine).toBe('cloud')
    // Re-running one is another request to the cloud GPU, and the consent
    // dialog asks for it first (`maskactions.svelte.js#rerunMask`).
    expect(row.reRunnable).toBe(true)
    expect(row.deletable).toBe(true)
  })

  it('names a patch the cloud rendered Cloud, as the native side records it: FLUX with a cloud record', () => {
    const remoteFlux = mask({
      provenance: {
        ...mask().provenance,
        engine: 'flux',
        cloud: { provider: 'beam', model: 'flux-schnell', request_id: 'r-flux', tier: 't4', cost: null },
      },
    })
    const [row] = maskRows([region({ mask: remoteFlux })])
    expect(row.engine).toBe('cloud')
    expect(row.titleKey).toBe('ladder.rung.cloud')
    expect(row.modelId).toBe('flux-schnell')
    expect(row.facts.map((fact) => fact.key)).toEqual([
      'masks.provenance.model', 'masks.provenance.cloudCost', 'masks.provenance.cloudRequestId',
    ])
    expect(row.reRunnable).toBe(true)
    expect(row.deletable).toBe(true)
    // subline does not contain fake zero cost
    expect(row.sub.map((s) => s.key)).not.toContain('masks.value.cloudCost')
  })

  it('keeps naming a local FLUX patch FLUX', () => {
    const [row] = maskRows([region({ mask: mask({ provenance: { ...mask().provenance, engine: 'flux' } }) })])
    expect(row.engine).toBe('flux')
    expect(row.titleKey).toBe('ladder.rung.flux')
  })
})

describe('maskRows - the review filter', () => {
  const flagged = [
    region({ id: 'a' }),
    region({ id: 'b', unusuallyLarge: true }),
    region({ id: 'c', mask: null, outcome: 'declined', declineReason: 'decline.reason.qualityMetric' }),
    region({ id: 'd', mask: null, outcome: 'gate-skipped', gateSkipCause: 'outside-bubble' }),
  ]

  it('lists exactly the review set, in the review set’s order', () => {
    const rows = maskRows(flagged, { filtered: true })
    expect(rows.map((row) => row.id)).toEqual(reviewList(flagged).map((entry) => entry.id))
    expect(rows.map((row) => row.id)).toEqual(['b', 'c', 'd'])
  })

  it('is a contiguous, order-preserving slice of the chapter set the pill steps through', () => {
    const otherPage = [region({ id: 'z', pageId: 'p2', unusuallyLarge: true })]
    const chapter = reviewList([...flagged, ...otherPage]).map((entry) => entry.id)
    const scoped = maskRows(flagged, { filtered: true }).map((row) => row.id)
    const at = chapter.indexOf(scoped[0])
    expect(chapter.slice(at, at + scoped.length)).toEqual(scoped)
  })

  it('swaps the sub-line for the reason it was flagged', () => {
    const rows = maskRows(flagged, { filtered: true })
    expect(rows[0].sub).toEqual([{ key: 'review.reason.unusuallyLarge' }])
    expect(rows.map((row) => row.reasonKey)).toEqual([
      'review.reason.unusuallyLarge',
      'review.reason.declined',
      'review.reason.gateSkippedOutsideBubble',
    ])
  })

  it('gives a gate-skip Clean anyway and a declined region only Show on page', () => {
    const rows = maskRows(flagged, { filtered: true })
    expect(rows[1].actions.map((action) => action.id)).toEqual(['showOnPage'])
    expect(rows[2].actions.map((action) => action.id)).toEqual(['showOnPage', 'cleanAnyway'])
  })

  it('distinguishes declined from every other flagged region by glyph and word', () => {
    const rows = maskRows(flagged, { filtered: true })
    expect(rows.map((row) => row.glyph)).toEqual(['△', '!', '△'])
    expect(rows[1].statusKey).toBe('masks.status.declined')
    expect(rows[0].statusKey).toBe('masks.status.needsReview')
  })
})

describe('maskRow facts', () => {
  it('omits redundant engine, model, fill, timing and origin facts', () => {
    const row = maskRow(region({ source: 'hand' }), false)
    expect(row.facts).toEqual([])
  })

  it('adds cost and request id for a cloud region, and flags nothing for having used the cloud', () => {
    const cloudMask = mask({
      // A legacy accepted outcome: it diagnosed nothing, and the row says so.
      cloudOutcome: { accepted: true, rejectionCause: null },
      provenance: {
        ...mask().provenance,
        engine: 'cloud',
        cloud: { provider: 'p', model: 'm', request_id: 'req-77', tier: 't', cost: 0.02 },
      },
    })
    const row = maskRow(region({ mask: cloudMask }), false)
    const keys = row.facts.map((fact) => fact.key)
    expect(keys).toContain('masks.provenance.cloudCost')
    expect(keys).toContain('masks.provenance.cloudRequestId')
    expect(row.facts.find((fact) => fact.key === 'masks.provenance.cloudCost')).toEqual({
      key: 'masks.provenance.cloudCost',
      valueKey: 'masks.value.cloudCost',
      params: { cost: 0.02 },
    })
    expect(keys).not.toContain('masks.provenance.flagged')
    expect(row.status).toBe('applied')
    expect(row.reasonKey).toBeNull()
    expect(row.facts.find((fact) => fact.key === 'masks.provenance.cloudRequestId').value).toBe(
      'req-77',
    )
  })

  it('ends with the reason when a cloud layer is flagged for a real cause', () => {
    const cloudMask = mask({
      cloudOutcome: { accepted: false, rejectionCause: 'residual-test' },
      provenance: { ...mask().provenance, engine: 'cloud', cloud: { provider: 'p', model: 'm', request_id: 'req-1' } },
    })
    const row = maskRow(region({ mask: cloudMask }), false)
    expect(row.status).toBe('review')
    expect(row.facts.at(-1)).toEqual({ key: 'masks.provenance.flagged', valueKey: 'review.reason.cloudRejectedResidualTest' })
  })

  it('says the cost was not reported when a cloud render came back without one', () => {
    const cloudMaskNullCost = mask({
      provenance: {
        ...mask().provenance,
        engine: 'flux',
        cloud: { provider: 'beam', model: 'flux-schnell', request_id: 'req-88', cost: null },
      },
    })
    const row = maskRow(region({ mask: cloudMaskNullCost }), false)
    expect(row.facts.find((fact) => fact.key === 'masks.provenance.cloudCost')).toEqual({
      key: 'masks.provenance.cloudCost',
      valueKey: 'masks.value.cloudCostUnknown',
    })
  })

  it('says what a region with no mask has had done to it', () => {
    const row = maskRow(
      region({ mask: null, outcome: 'gate-skipped', gateSkipCause: 'low-confidence', detected: false }),
      true,
    )
    expect(row.titleKey).toBe('masks.title.gateSkipped')
    // Deletable like every other row: a warning with no way off the list is a
    // list that only grows. What goes is the region, not a mask it never had.
    expect(row.deletable).toBe(true)
    expect(row.engine).toBe(null)
    expect(row.reRunnable).toBe(false)
    expect(row.facts).toEqual([
      { key: 'masks.provenance.applied', valueKey: 'masks.value.nothingApplied' },
      { key: 'masks.provenance.detected', valueKey: 'masks.value.detectedNo' },
      {
        key: 'masks.provenance.flagged',
        valueKey: 'review.reason.gateSkippedLowConfidence',
      },
    ])
  })

  it('calls a region with no mask and nothing wrong with it unexamined, not applied', () => {
    const row = maskRow(region({ mask: null, detected: false }), false)
    expect(row.status).toBe('unexamined')
    expect(row.statusKey).toBe('masks.status.unexamined')
    expect(row.glyph).toBe('○')
    expect(row.titleKey).toBe('masks.title.noMask')
  })

  it('keeps applied for a region that actually has a mask', () => {
    expect(maskRow(region(), false).status).toBe('applied')
  })
})

describe('flaggedCount', () => {
  it('counts the regions the filter would show', () => {
    const regions = [region(), region({ id: 'b', unusuallyLarge: true })]
    expect(flaggedCount(regions)).toBe(1)
    expect(flaggedCount(regions)).toBe(maskRows(regions, { filtered: true }).length)
  })
})

describe('a detected region', () => {
  const detection = (id, extra = {}) => region({ id, outcome: 'detected', pick: 'fill', detector: 'cloud', ...extra })

  it('is a Detected row, after the masks and before the unexamined, with Clean and Clean on cloud GPU', () => {
    const rows = maskRows([region({ id: 'u', outcome: 'pending', mask: null }), detection('d'), region({ id: 'c' })])
    expect(rows.map((row) => row.status)).toEqual(['applied', 'detected', 'unexamined'])
    const row = rows[1]
    expect(row).toMatchObject({ glyph: '◇', statusKey: 'masks.status.detected', titleKey: 'masks.title.detected', reRunnable: false })
    expect(row.actions.map((action) => action.id)).toEqual(['cleanDetected', 'cleanDetectedCloud'])
  })

  it('is not flagged for review', () => {
    expect(flaggedCount([detection('d', { unusuallyLarge: true })])).toBe(0)
  })

  it('saved with the retired denoise pick starts on Fill', () => {
    const row = maskRow(detection('d', { pick: 'denoise' }), false)
    expect(row.sub).toContainEqual({ key: 'masks.sub.startsWith', params: { engineKey: 'masks.engineChoice.fill' } })
    expect(row.facts).toContainEqual({ key: 'masks.provenance.startsWith', valueKey: 'masks.engineChoice.fill' })
  })

  it('whose committed cloud result went missing is flagged with its reason and can still be cleaned', () => {
    const flagged = detection('d', { attention: 'review.reason.repairNeeded' })
    const row = maskRow(flagged, false)
    // Still a detection to clean: the status, glyph and words of one, so the
    // cloud action and the canvas treat it as one. The flag rides beside it.
    expect(row).toMatchObject({
      status: 'detected',
      glyph: '◇',
      statusKey: 'masks.status.detected',
      reasonKey: 'review.reason.repairNeeded',
      titleKey: 'masks.title.detected',
    })
    expect(row.sub[0]).toEqual({ key: 'review.reason.repairNeeded' })
    expect(row.facts).toContainEqual({ key: 'masks.provenance.flagged', valueKey: 'review.reason.repairNeeded' })
    expect(row.actions.map((action) => action.id)).toEqual(['cleanDetected', 'cleanDetectedCloud'])
    expect(flaggedCount([flagged])).toBe(1)
    // And in the review filter, as the same detected row.
    expect(maskRows([flagged, detection('plain')], { filtered: true }))
      .toEqual([expect.objectContaining({ id: 'd', status: 'detected', reasonKey: 'review.reason.repairNeeded' })])
  })
})

describe('candidates in Layers', () => {
  const candidate = (id, reason = 'review.reason.unassignedMask') =>
    region({ id, outcome: 'candidate', candidateReason: reason, mask: null })

  it('are listed as candidates with their evidence reason, not as declined failures', () => {
    const row = maskRow(candidate('c1', 'review.reason.isolatedMask'), false)
    expect(row.status).toBe('candidate')
    expect(row.glyph).toBe('◌')
    expect(row.statusKey).toBe('review.state.candidate')
    expect(row.titleKey).toBe('review.candidate.title')
    expect(row.sub).toEqual([{ key: 'review.reason.isolatedMask' }])
    expect(row.reasonKey).toBeNull()
    expect(row.candidateReasonKey).toBe('review.reason.isolatedMask')
    expect(row.facts).toContainEqual({ key: 'review.candidate.heldBecause', valueKey: 'review.reason.isolatedMask' })
  })

  it('can be cleaned or found on the page from the row, and dismissed like any row', () => {
    const row = maskRow(candidate('c1'), false)
    expect(row.actions.map((action) => action.id)).toEqual(['cleanAnyway', 'showOnPage'])
    expect(row.actions[0]).toMatchObject({ labelKey: 'review.candidate.clean', hintKey: 'review.candidate.cleanHint' })
    expect(row.deletable).toBe(true)
  })

  it('sit together after the work still to clean, and stay out of the review filter and count', () => {
    const regions = [
      candidate('cand'),
      region({ id: 'gate', mask: null, outcome: 'gate-skipped', gateSkipCause: 'outside-bubble' }),
      region({ id: 'det', outcome: 'detected' }),
      region({ id: 'layer' }),
    ]
    expect(maskRows(regions).map((row) => row.id)).toEqual(['layer', 'det', 'cand', 'gate'])
    expect(maskRows(regions, { filtered: true }).map((row) => row.id)).toEqual(['gate'])
    expect(flaggedCount(regions)).toBe(1)
    expect(candidateCount(regions)).toBe(1)
  })
})

describe('failed and needs-review rows', () => {
  it('a decline is a failure, and says its cause', () => {
    const row = maskRow(region({ id: 'd', mask: null, outcome: 'declined', declineReason: 'decline.reason.rungUnavailable' }), false)
    expect(row.status).toBe('declined')
    expect(row.reasonKey).toBe('review.reason.declined')
    expect(row.facts).toContainEqual({ key: 'review.fact.cause', valueKey: 'decline.reason.rungUnavailable' })
  })

  it('a held text box with no lettering under it needs review for its mask', () => {
    const row = maskRow(region({ id: 'm', mask: null, outcome: 'declined', declineReason: 'review.reason.maskMissingUnderBox' }), false)
    expect(row.status).toBe('review')
    expect(row.sub).toEqual([{ key: 'review.reason.maskMissingUnderBox' }])
  })

  it('a cleaned layer whose lettering crosses its balloon needs review, with that reason', () => {
    const row = maskRow(region({ id: 'x', mask: mask({ id: 'x-m1', regionId: 'x', maskReview: 'review.reason.crossesBalloon' }) }), false)
    expect(row.status).toBe('review')
    expect(row.reasonKey).toBe('review.reason.crossesBalloon')
  })

  it('shows the generated texture check and what to inspect on a marked layer', () => {
    const row = maskRow(region({ mask: mask({ generatedTextureReview: true }) }), true)
    expect(row.status).toBe('review')
    expect(row.reasonKey).toBe('review.reason.checkGeneratedTexture')
    expect(row.sub).toEqual([{ key: 'review.reason.checkGeneratedTexture' }])
    expect(row.facts).toContainEqual({ key: 'review.fact.detail', valueKey: 'review.detail.generatedTexture' })
  })
})
