/**
 * The browser mock's Detect, Clean and cloud clean, held to docs/detect-clean.md:
 * Detect stores regions and cleans nothing, Clean turns exactly those into
 * cleaned ones, and a cloud clean is prepared, confirmed and started as an
 * ordinary run whose failed region stays detected.
 */

import { describe, expect, it } from 'vitest'

import { maskRow } from '../editor/maskrows.js'
import { cloudCropPixels, createMockBackend } from './mock.js'

const CHAPTER = 'tsuki-to-hane-ch107'
const MODAL_KEYS = { token_id: 'ak-fake', token_secret: 'as-fake' }
const ANSWER = { rightsAttested: true, retentionAcknowledged: true }

function makeMock(options = {}) {
  return createMockBackend({ timing: { method: 0, cloud: 0, provision: 0, region: 0, pageTail: 0 }, ...options })
}

/** A mock with cloud allowed and a provisioned Modal endpoint selected. */
async function readyMock(options = {}) {
  const mock = makeMock(options)
  await mock.writeSettings({ cloudEngines: 'allowed' })
  const plan = await mock.runCloudProvisioner({ op: 'plan', provider: 'modal',
    params: { credentials: MODAL_KEYS, installation_id: 'mc-detect1', options: {} } })
  const applied = await mock.runCloudProvisioner({ op: 'apply', provider: 'modal',
    params: { credentials: MODAL_KEYS, installation_id: 'mc-detect1', approved_plan_hash: plan.data.plan_hash } })
  expect(applied.success).toBe(true)
  return mock
}

/**
 * Start a run and collect its events up to `run-finished`.
 *
 * @param {any} mock
 * @param {() => Promise<any>} start
 */
async function ranToEnd(mock, start) {
  const events = []
  let done = () => {}
  const finished = new Promise((resolve) => { done = resolve })
  const stop = mock.subscribe((event) => {
    events.push(event)
    if (event.type === 'run-finished') done()
  })
  const handle = await start()
  if (handle?.runId) await finished
  stop()
  return { handle, events }
}

/** @param {any} mock */
async function firstPage(mock) {
  const [page] = await mock.loadPages({ chapterId: CHAPTER, indices: [0] })
  return page
}

const runOn = (mode) => ({ scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode })

describe('Detect and Clean', () => {
  it('detects without cleaning, then cleans exactly what was detected', async () => {
    const mock = makeMock()
    const before = await firstPage(mock)
    const queued = before.regions.filter((region) => region.outcome === 'pending').map((region) => region.id)
    expect(queued.length).toBeGreaterThan(0)

    const detect = await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    expect(detect.events).toContainEqual(expect.objectContaining({ type: 'run-finished', reason: 'completed' }))
    expect(detect.events).toContainEqual(expect.objectContaining({ type: 'notice', key: 'notice.run.detected',
      params: expect.objectContaining({ regions: queued.length }) }))
    // Detections arrive with their page, not as regions done.
    expect(detect.events.some((event) => event.type === 'region-done')).toBe(false)
    const pageDone = detect.events.find((event) => event.type === 'page-done')
    expect(pageDone.page.regions.filter((region) => region.outcome === 'detected')).toHaveLength(queued.length)
    const detected = await firstPage(mock)
    expect(detected.status).toBe('detected')
    for (const id of queued) {
      const region = detected.regions.find((candidate) => candidate.id === id)
      expect(region).toMatchObject({ outcome: 'detected', detected: true, source: 'auto', detector: 'local' })
      expect(region.mask).toBeTruthy()
      expect(['fill', 'solid', 'lama']).toContain(region.pick)
    }

    const clean = await ranToEnd(mock, () => mock.runClean(runOn('clean')))
    expect(clean.events.filter((event) => event.type === 'region-done')).toHaveLength(queued.length)
    const cleaned = await firstPage(mock)
    expect(cleaned.status).toBe('cleaned')
    for (const id of queued) {
      const region = cleaned.regions.find((candidate) => candidate.id === id)
      expect(region.outcome).toBe('cleaned')
      expect(region.pick).toBeUndefined()
    }
  })

  it('cleans a page with detections rather than detecting it again, in Detect and clean', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const detected = (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')
    const auto = await ranToEnd(mock, () => mock.runClean(runOn('auto')))
    const done = auto.events.filter((event) => event.type === 'region-done').map((event) => event.region.id)
    expect(done.sort()).toEqual(detected.map((region) => region.id).sort())
    expect((await firstPage(mock)).status).toBe('cleaned')
  })

  it('says there is nothing detected when Clean has nothing to clean, and starts nothing', async () => {
    const mock = makeMock()
    const { handle, events } = await ranToEnd(mock, () => mock.runClean(runOn('clean')))
    expect(handle.runId).toBeNull()
    expect(events).toContainEqual(expect.objectContaining({ type: 'notice', key: 'notice.run.nothingDetected' }))
  })

  it('cleans one detected region on its own, and deletes one without a trace', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const [first, second] = (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')
    const applied = await mock.applyTool({ tool: 'autoClean', chapterId: CHAPTER, pageIndex: 0, regionId: first.id, params: {} })
    expect(applied).toMatchObject({ status: 'applied', region: { id: first.id, outcome: 'cleaned' } })

    if (second) {
      const deleted = await mock.deleteMask({ maskId: second.mask.id })
      expect(deleted.region).toBeNull()
      expect((await firstPage(mock)).regions.some((region) => region.id === second.id)).toBe(false)
    }
  })
})

/**
 * The selection tool's seam in the mock: an approximation on boxes, because
 * the mock has no pixels (`mock.js#editDetectionMask`). What must hold is the
 * native command's shape - add grows the detection it overlaps most or makes
 * a new one, remove deletes what it covers, a cleaned layer never moves, and
 * the answer names every region it touched.
 */
describe('editing the detected masks', () => {
  /** A point on the page no region's box comes near, in page percent. */
  const emptySpot = (page) => {
    for (let y = 2; y < 98; y += 2) {
      for (let x = 2; x < 98; x += 2) {
        const clear = page.regions.every(({ bbox }) =>
          x < bbox.x - 3 || x > bbox.x + bbox.w + 3 || y < bbox.y - 3 || y > bbox.y + bbox.h + 3)
        if (clear) return { x, y }
      }
    }
    throw new Error('no empty spot on the fixture page')
  }
  const centre = (bbox) => ({ x: bbox.x + bbox.w / 2, y: bbox.y + bbox.h / 2 })
  const edit = (mock, spec) => mock.editDetectionMask({ chapterId: CHAPTER, pageIndex: 0, ...spec })

  it('grows the detection a stroke overlaps, and says which', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const [target, ...others] = (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')
    const at = centre(target.bbox)
    const answer = await edit(mock, { mode: 'add', stroke: { points: [at, { x: at.x + target.bbox.w, y: at.y }], radius: 8 } })
    expect(answer).toMatchObject({ pageStatus: 'detected', changed: [target.id], created: [], removed: [] })

    const page = await firstPage(mock)
    const grown = page.regions.find((region) => region.id === target.id)
    expect(grown.outcome).toBe('detected')
    expect(grown.bbox.x + grown.bbox.w).toBeGreaterThan(target.bbox.x + target.bbox.w)
    // A new digest, so the mask's URL moves as the native one would.
    expect(grown.mask.provenance.mask_sha256).not.toBe(target.mask.provenance.mask_sha256)
    for (const other of others) {
      expect(page.regions.find((region) => region.id === other.id)?.bbox).toEqual(other.bbox)
    }
  })

  it('makes a new detection where the gesture touches none', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const before = await firstPage(mock)
    const spot = emptySpot(before)
    const painted = { kind: 'rect', feather: 0, points: [spot, { x: spot.x + 1, y: spot.y }, { x: spot.x + 1, y: spot.y + 1 }, { x: spot.x, y: spot.y + 1 }] }
    const answer = await edit(mock, { mode: 'add', painted })
    expect(answer.created).toHaveLength(1)
    expect(answer.changed).toEqual([])

    const made = (await firstPage(mock)).regions.find((region) => region.id === answer.created[0])
    expect(made).toMatchObject({ outcome: 'detected', source: 'hand', bbox: { x: spot.x, y: spot.y, w: 1, h: 1 } })
    expect(made.mask).toBeTruthy()
    expect(['fill', 'solid', 'lama']).toContain(made.pick)
    // It is a detection like the others: Clean cleans it.
    const clean = await ranToEnd(mock, () => mock.runClean(runOn('clean')))
    expect(clean.events.some((event) => event.type === 'region-done' && event.region.id === made.id)).toBe(true)
  })

  it('removes the detections it covers and settles the page once none are left', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const detected = (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')
    const [first] = detected
    const at = centre(first.bbox)
    const answer = await edit(mock, { mode: 'remove', stroke: { points: [at], radius: 4 } })
    expect(answer.removed).toContain(first.id)
    expect(answer.changed).toEqual([])
    expect((await firstPage(mock)).regions.some((region) => region.id === first.id)).toBe(false)

    const all = await edit(mock, { mode: 'remove', painted: { kind: 'rect', feather: 0,
      points: [{ x: 0, y: 0 }, { x: 100, y: 0 }, { x: 100, y: 100 }, { x: 0, y: 100 }] } })
    expect(all.pageStatus).not.toBe('detected')
    const page = await firstPage(mock)
    expect(page.status).toBe(all.pageStatus)
    expect(page.regions.some((region) => region.outcome === 'detected')).toBe(false)
  })

  it('never touches a cleaned layer', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const [first] = (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')
    await mock.applyTool({ tool: 'autoClean', chapterId: CHAPTER, pageIndex: 0, regionId: first.id, params: {} })
    const cleaned = (await firstPage(mock)).regions.find((region) => region.id === first.id)
    expect(cleaned.outcome).toBe('cleaned')

    const at = centre(cleaned.bbox)
    const answer = await edit(mock, { mode: 'remove', stroke: { points: [at], radius: 2 } })
    expect(answer?.removed ?? []).not.toContain(first.id)
    expect((await firstPage(mock)).regions.find((region) => region.id === first.id)).toEqual(cleaned)
  })

  it('refuses a mode it does not know, and answers null for a page or source that is not the one drawn', async () => {
    const mock = makeMock()
    const stroke = { points: [{ x: 50, y: 50 }], radius: 8 }
    await expect(edit(mock, { mode: 'grow', stroke })).rejects.toThrow('mask_edit_mode_invalid')
    expect(await mock.editDetectionMask({ chapterId: CHAPTER, pageIndex: 999, mode: 'add', stroke })).toBeNull()
    const page = await firstPage(mock)
    expect(await edit(mock, { mode: 'add', stroke, sourceIndex: page.sourceIndex, sourceSha: 'not-this-scan' })).toBeNull()
    expect(await edit(mock, { mode: 'add' })).toBeNull()
  })
})

/**
 * Mask padding in the mock, on boxes as the mask edit is: Detect grows each
 * detection by the run's padding, and `setDetectionPadding` re-pads stored
 * ones from their unpadded box, so 0 gives that box back.
 */
describe('padding the detected masks', () => {
  const detected = async (mock) => (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')
  const pad = (mock, spec) => mock.setDetectionPadding({ chapterId: CHAPTER, ...spec })

  it('grows what Detect stores by the run\'s padding', async () => {
    const plain = makeMock()
    await ranToEnd(plain, () => plain.runClean(runOn('detect')))
    const padded = makeMock()
    await ranToEnd(padded, () => padded.runClean({ ...runOn('detect'), maskPaddingPx: 12 }))
    const [before, after] = [await detected(plain), await detected(padded)]
    expect(after.map((region) => region.id)).toEqual(before.map((region) => region.id))
    for (const [index, region] of after.entries()) {
      expect(region.bbox.w).toBeGreaterThan(before[index].bbox.w)
      expect(region.baseBbox).toEqual(before[index].bbox)
    }
  })

  it('re-pads stored detections from their unpadded box, and 0 gives it back', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const original = await detected(mock)
    const wide = await pad(mock, { pageIndex: 0, paddingPx: 16 })
    expect(wide).toEqual({ changed: original.map((region) => region.id), removed: [], pages: [0] })
    const grown = await detected(mock)
    expect(grown[0].bbox.w).toBeGreaterThan(original[0].bbox.w)
    expect(grown[0].mask.provenance.mask_sha256).not.toBe(original[0].mask.provenance.mask_sha256)

    expect(await pad(mock, { pageIndex: 0, paddingPx: 16 })).toEqual({ changed: [], removed: [], pages: [] })
    await pad(mock, { paddingPx: 4 })
    await pad(mock, { paddingPx: 0 })
    expect((await detected(mock)).map((region) => region.bbox)).toEqual(original.map((region) => region.bbox))
  })

  it('limits padding to one detection and rejects a target outside the scope', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const original = await detected(mock)
    expect(original.length).toBeGreaterThan(1)
    const regionId = original[0].id
    for (const spec of [{ regionId: 'missing', pageIndex: 0 }, { regionId, pageIndex: 999 }]) {
      await expect(pad(mock, { ...spec, paddingPx: 4 })).rejects.toThrow('mask_padding_target_invalid')
      expect(await detected(mock)).toEqual(original)
    }
    expect(await pad(mock, { regionId, pageIndex: 0, paddingPx: 4 })).toEqual({ changed: [regionId], removed: [], pages: [0] })
    const after = await detected(mock)
    expect(after[0].paddingPx).toBe(4)
    expect(after.slice(1)).toEqual(original.slice(1))
  })

  it('refuses a padding the native side would, and answers null for a chapter that is not there', async () => {
    const mock = makeMock()
    for (const paddingPx of [-1, 33, 2.5, undefined]) {
      await expect(pad(mock, { pageIndex: 0, paddingPx })).rejects.toThrow('mask_padding_invalid')
    }
    await expect(mock.runClean({ ...runOn('detect'), maskPaddingPx: 64 })).rejects.toThrow('mask_padding_invalid')
    expect(await mock.setDetectionPadding({ chapterId: 'no-such-chapter', paddingPx: 2 })).toBeNull()
  })
})

/**
 * The region menu's Text type in the mock, as `library.rs#retype_detection`
 * sets it natively: the balloon answer flips, a Fill pick moved outside a
 * balloon becomes LaMa and a LaMa pick moved inside stays, and only a stored
 * detection has a type to set.
 */
describe('a detection\'s text type', () => {
  const retype = (mock, regionId, inside) => mock.setDetectionType({ regionId, inside })

  it('flips the balloon answer, and moves a Fill pick to LaMa only outside a balloon', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const bubble = (await firstPage(mock)).regions
      .find((region) => region.outcome === 'detected' && region.insideBubble === true && region.pick === 'fill')
    expect(bubble, 'a detection inside a bubble, starting on Fill').toBeTruthy()

    const outside = await retype(mock, bubble.id, false)
    expect(outside).toMatchObject({ id: bubble.id, outcome: 'detected', insideBubble: false, pick: 'lama' })
    expect(outside.mask.provenance.engine).toBe('lama')
    expect(outside.mask.fillMode).toBe('reconstruct')
    const stored = (await firstPage(mock)).regions.find((region) => region.id === bubble.id)
    expect(stored).toEqual(outside)
    expect(stored.bbox).toEqual(bubble.bbox)

    const inside = await retype(mock, bubble.id, true)
    expect(inside).toMatchObject({ insideBubble: true, pick: 'lama' })
    expect(await retype(mock, bubble.id, true)).toEqual(inside)
    expect((await firstPage(mock)).status).toBe('detected')
  })

  it('refuses a region that is not a stored detection', async () => {
    const mock = makeMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const [first] = (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')
    await mock.applyTool({ tool: 'autoClean', chapterId: CHAPTER, pageIndex: 0, regionId: first.id, params: {} })
    const cleaned = (await firstPage(mock)).regions.find((region) => region.id === first.id)
    expect(cleaned.outcome).toBe('cleaned')
    await expect(retype(mock, first.id, false)).rejects.toThrow('not_a_detection')
    await expect(retype(mock, 'no-such-region', true)).rejects.toThrow('not_a_detection')
    expect((await firstPage(mock)).regions.find((region) => region.id === first.id)).toEqual(cleaned)
  })
})

describe('cloud clean', () => {
  it('estimates a crop in pixels, including context, and rejects an oversized crop', () => {
    expect(cloudCropPixels({ bbox: { x: 10, y: 10, w: 10, h: 10 } }, { width: 1000, height: 1000 })).toBe(368 * 368)
    expect(cloudCropPixels({ bbox: { x: 0, y: 0, w: 90, h: 90 } }, { width: 2400, height: 2400 })).toBeNull()
  })

  it('prepares a description only, confirms, and renders as an ordinary run', async () => {
    const mock = await readyMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const detectedIds = (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected').map((r) => r.id)

    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0] })
    // Strictly cloud unless mixed is asked for, and preparing cleans nothing.
    expect(proposal).toMatchObject({ execution: 'cloud', localCleaned: 0, localCandidates: 0, chunkRegions: 256 })
    expect(proposal.regions).toBe(detectedIds.length)
    expect((await firstPage(mock)).regions.filter((region) => region.outcome === 'detected').map((r) => r.id))
      .toEqual(detectedIds)
    if (!proposal.regions) return
    expect(proposal).toMatchObject({ chapterId: CHAPTER, provider: 'modal', gpu: 'L4', pages: 1, chunks: 1 })
    expect(proposal.estimatedCostUsd.high).toBeGreaterThanOrEqual(proposal.estimatedCostUsd.low)
    await expect(mock.confirmCloudClean({ proposalId: proposal.proposalId, rightsAttested: true, retentionAcknowledged: false }))
      .rejects.toThrow('retention_acknowledgement_required')

    const again = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    const grant = await mock.confirmCloudClean({ proposalId: again.proposalId, planDigest: again.planDigest, ...ANSWER })
    const { events } = await ranToEnd(mock, () => mock.startCloudClean({ grantId: grant.grantId }))
    expect(events.filter((event) => event.type === 'region-done').map((event) => event.region.id).sort())
      .toEqual([...again.regionIds].sort())
    const page = await firstPage(mock)
    for (const id of again.regionIds) {
      expect(page.regions.find((region) => region.id === id)?.mask.provenance.cloud).toBeTruthy()
    }
    // A grant is spent once.
    await expect(mock.startCloudClean({ grantId: grant.grantId })).rejects.toThrow('cloud_clean_grant_missing')
  })

  it('describes mixed execution at prepare and cleans flat colours here only once the run starts', async () => {
    const mock = await readyMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const before = (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected').map((r) => r.id)
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: true })
    expect(proposal).toMatchObject({ execution: 'mixed', localCleaned: 0, regions: before.length })
    expect((await firstPage(mock)).regions.filter((region) => region.outcome === 'detected').map((r) => r.id))
      .toEqual(before)
    if (!proposal.regions) return
    const grant = await mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER })
    const { events } = await ranToEnd(mock, () => mock.startCloudClean({ grantId: grant.grantId }))
    expect(events.filter((event) => event.type === 'region-done').map((event) => event.region.id).sort())
      .toEqual([...proposal.regionIds].sort())
  })

  it('holds a region too large for the render service out of the plan, as the native side does', async () => {
    const mock = await readyMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const clear = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0] })
    expect(clear.tooLargeIds).toEqual([])
    const detected = (await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')
    if (!detected.length) return
    // Nine tenths of a 2400 px page tall: past 2048 px with no context at all.
    const big = { ...detected[0], bbox: { x: 10, y: 5, w: 20, h: 90 } }
    await mock.restoreRegion({ regionId: big.id, region: big })

    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0] })
    expect(proposal.tooLargeIds).toEqual([big.id])
    expect(proposal.regionIds).not.toContain(big.id)
    expect(proposal.regions).toBe(detected.length - 1)
    const only = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'regions', regionIds: [big.id] })
    expect(only).toMatchObject({ proposalId: null, regions: 0, tooLargeIds: [big.id], planDigest: null })
    await expect(mock.prepareCloudConsent({
      regionId: big.id,
      target: { type: 'modal', profile_id: (await mock.readInferenceConfig()).selectedTarget.profile_id },
    })).rejects.toThrow('cloud_consent_crop_too_large')
  })

  it('mints a grant only for the plan the consent showed', async () => {
    const mock = await readyMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    if (!proposal.regions) return
    await expect(mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: 'f'.repeat(64), ...ANSWER }))
      .rejects.toThrow('cloud_clean_plan_mismatch')
    // Spent, as the native side spends it.
    await expect(mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER }))
      .rejects.toThrow('cloud_clean_proposal_missing')
    const again = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    await expect(mock.confirmCloudClean({ proposalId: again.proposalId, ...ANSWER })).rejects.toThrow('cloud_clean_plan_mismatch')
  })

  it('drops a declined proposal, and refuses while cloud engines are off', async () => {
    const mock = await readyMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'chapter', localFirst: false })
    expect(await mock.cancelCloudClean({ proposalId: proposal.proposalId })).toBe(true)
    await expect(mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER })).rejects.toThrow('cloud_clean_proposal_missing')

    await mock.writeSettings({ cloudEngines: 'blocked' })
    await expect(mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'chapter' })).rejects.toThrow('cloud_disabled')
  })

  it('leaves a failed region detected, with a notice, and cleans the rest', async () => {
    const mock = await readyMock({ cloudCleanScenario: 'regionFail' })
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    const grant = await mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER })
    const { events } = await ranToEnd(mock, () => mock.startCloudClean({ grantId: grant.grantId }))
    expect(events).toContainEqual(expect.objectContaining({ type: 'notice', key: 'notice.cloudClean.regionFailed',
      params: { regionId: proposal.regionIds[0], page: expect.any(Number), code: 'gateway_unreachable' } }))
    const page = await firstPage(mock)
    const left = page.regions.filter((region) => region.outcome === 'detected')
    expect(left.map((region) => region.id)).toEqual([proposal.regionIds[0]])
    expect(page.status).toBe('detected')
  })

  it('refuses to start under the startFail knob, leaving every region detected', async () => {
    const mock = await readyMock({ cloudCleanScenario: 'startFail' })
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    const grant = await mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER })
    await expect(mock.startCloudClean({ grantId: grant.grantId })).rejects.toThrow('credential_missing')
    expect((await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')).toHaveLength(proposal.regions)
  })

  it('answers in the native shape: page indices, and no proposal and no expiry when nothing is left', async () => {
    const mock = await readyMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    expect(proposal.pageIndices).toEqual([0])
    expect(proposal.pages).toBe(1)
    await mock.cancelCloudClean({ proposalId: proposal.proposalId })
    const fresh = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    const grant = await mock.confirmCloudClean({ proposalId: fresh.proposalId, planDigest: fresh.planDigest, ...ANSWER })
    await ranToEnd(mock, () => mock.startCloudClean({ grantId: grant.grantId }))
    const empty = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0] })
    expect(empty).toMatchObject({ proposalId: null, regions: 0, pages: 0, pageIndices: [], expiresAtMs: 0 })
  })

  it('refuses with the native codes', async () => {
    const mock = await readyMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    await expect(mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [] })).rejects.toThrow('cloud_clean_no_pages')
    await expect(mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [999] })).rejects.toThrow('cloud_clean_page_not_found')
    await expect(mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'regions', regionIds: [] })).rejects.toThrow('cloud_clean_no_regions')
    await expect(mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'regions', regionIds: ['nope'] })).rejects.toThrow('cloud_clean_region_not_found')
    await expect(mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'project' })).rejects.toThrow('cloud_clean_scope_unsupported')
    await mock.writeInferenceConfig({ config: { ...(await mock.readInferenceConfig()), selectedTarget: { type: 'local' } } })
    await expect(mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'chapter' })).rejects.toThrow('cloud_clean_profile_not_active')
  })

  it('answers a busy slot with that run and leaves the grant unspent', async () => {
    const mock = await readyMock({ timing: { method: 0, cloud: 0, provision: 0, region: 20, pageTail: 0 } })
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    const grant = await mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER })
    const other = await mock.runClean({ scope: 'chapter', chapterId: CHAPTER, pageIndex: 0, mode: 'auto' })
    expect(await mock.startCloudClean({ grantId: grant.grantId })).toEqual({ runId: other.runId, pages: [], alreadyRunning: true })
    await mock.cancelRun({ runId: other.runId })
    const started = await mock.startCloudClean({ grantId: grant.grantId })
    expect(started.runId).toBeTruthy()
    expect(started.alreadyRunning).toBeUndefined()
  })

  it('stops the batch with one notice under the stop knob, leaving every region detected', async () => {
    const mock = await readyMock({ cloudCleanScenario: 'stop' })
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    const grant = await mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER })
    const { events } = await ranToEnd(mock, () => mock.startCloudClean({ grantId: grant.grantId }))
    const stops = events.filter((event) => event.type === 'notice' && event.key === 'notice.cloudClean.stopped')
    expect(stops).toEqual([expect.objectContaining({ params: { page: 1, code: 'gateway_unauthorized' } })])
    expect((await firstPage(mock)).regions.filter((region) => region.outcome === 'detected')).toHaveLength(proposal.regions)
  })

  it('leaves out a region deleted after the consent, with the native notice', async () => {
    const mock = await readyMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    const grant = await mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER })
    const gone = (await firstPage(mock)).regions.find((region) => region.id === proposal.regionIds[0])
    await mock.deleteMask({ maskId: gone.mask.id })
    const { events } = await ranToEnd(mock, () => mock.startCloudClean({ grantId: grant.grantId }))
    expect(events).toContainEqual(expect.objectContaining({ type: 'notice', key: 'notice.cloudClean.regionSkipped',
      params: { regionId: gone.id, page: 1, reason: 'gone' } }))
  })

  it('patches arrive as FLUX with the cloud record, which the Layers row reads as the cloud', async () => {
    const mock = await readyMock()
    await ranToEnd(mock, () => mock.runClean(runOn('detect')))
    const proposal = await mock.prepareCloudClean({ chapterId: CHAPTER, scope: 'page', pageIndices: [0], localFirst: false })
    const grant = await mock.confirmCloudClean({ proposalId: proposal.proposalId, planDigest: proposal.planDigest, ...ANSWER })
    await ranToEnd(mock, () => mock.startCloudClean({ grantId: grant.grantId }))
    const region = (await firstPage(mock)).regions.find((candidate) => candidate.id === proposal.regionIds[0])
    expect(region.outcome).toBe('cleaned')
    expect(region.mask.provenance.engine).toBe('flux')
    expect(region.mask.provenance.cloud).toBeTruthy()
    expect(maskRow(region, false)).toMatchObject({ status: 'applied', engine: 'cloud' })
  })
})
