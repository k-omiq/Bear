/**
 * The run scheduler, and only the run scheduler - the one piece of this layer
 * with logic rather than data. Fixture contents are not tested (they are
 * data), and neither are the typedefs.
 *
 * Timings are injected so fake timers drive the whole run: a page costs
 * `region` ms per pending region plus `pageTail` ms.
 */

import { inflateSync } from 'node:zlib'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { readFileSync } from 'node:fs'
import { ANALYSIS_PROTOCOL_VERSION, CLOUD_ANALYSIS_SPATIAL, cloudTilePlan, createMockBackend } from './mock.js'
import { maskRows } from '../editor/maskrows.js'

const TIMING = { method: 0, openChapter: 0, export: 0, region: 100, pageTail: 100, noticeStagger: 0 }

/** A chapter with 20 pages, none of them cleaned. */
const CHAPTER = 'tsuki-to-hane-ch107'
/** A chapter of 48 pages with no text detected on any of them. */
const EMPTY_CHAPTER = 'yoake-photobook-ch1'

function makeBackend() {
  const backend = createMockBackend({ timing: TIMING })
  /** @type {Object[]} */
  const events = []
  backend.subscribe((event) => events.push(event))
  return { backend, events }
}

/** Resolves a backend promise, flushing every timer it waits on. */
async function settle(promise) {
  // Attach rejection handling before advancing timers that may reject it.
  const observed = promise.then(
    (value) => ({ value }),
    (error) => ({ error }),
  )
  await vi.runAllTimersAsync()
  const result = await observed
  if ('error' in result) throw result.error
  return result.value
}

/** Resolves a backend promise without running the run it may have started. */
async function begin(promise) {
  await vi.advanceTimersByTimeAsync(0)
  return promise
}

beforeEach(() => vi.useFakeTimers())
afterEach(() => vi.useRealTimers())

/**
 * Every page of a chapter, paged in.
 *
 * A chapter crosses the seam as page **headers** -
 * the regions arrive only for the window somebody is looking at - so a test
 * that wants to reason about a chapter's regions has to ask for them, exactly
 * as the editor does.
 */
async function pagedIn(backend, chapter, settle) {
  return settle(
    backend.loadPages({ chapterId: chapter.id, indices: chapter.pages.map((p) => p.index) }),
  )
}

describe('cloud analysis tile plan', () => {
  it('matches the plan the native side and the gateway share', () => {
    const fixture = JSON.parse(readFileSync(new URL('../../../deploy/cloud/fixtures/analysis_v1/tile_plans.json', import.meta.url), 'utf8'))
    for (const { width, height, tiles } of fixture.plans) {
      expect(cloudTilePlan(width, height), `${width}x${height}`).toEqual(tiles)
    }
    const vectors = JSON.parse(readFileSync(new URL('../../../deploy/cloud/fixtures/analysis_v1/vectors.json', import.meta.url), 'utf8'))
    expect(ANALYSIS_PROTOCOL_VERSION).toBe(vectors.valid[0].request.protocol_version)
    expect(CLOUD_ANALYSIS_SPATIAL).toBe(fixture.spatial)
  })
})

describe('run scheduler', () => {
  it('emits every page in order and finishes', async () => {
    const { backend, events } = makeBackend()

    const handle = await settle(backend.runClean({ scope: 'chapter', chapterId: CHAPTER }))

    expect(handle.pages).toHaveLength(20)

    const started = events.filter((e) => e.type === 'page-started').map((e) => e.pageIndex)
    const done = events.filter((e) => e.type === 'page-done').map((e) => e.pageIndex)
    const queued = handle.pages.map((page) => page.pageIndex)
    expect(started).toEqual(queued)
    expect(done).toEqual(queued)

    // Every region of a page is emitted between that page's start and its end.
    for (const pageIndex of queued) {
      const openedAt = events.findIndex((e) => e.type === 'page-started' && e.pageIndex === pageIndex)
      const closedAt = events.findIndex((e) => e.type === 'page-done' && e.pageIndex === pageIndex)
      const regions = events.filter((e) => e.type === 'region-done' && e.pageId === events[closedAt].pageId)
      expect(regions.length).toBeGreaterThan(0)
      for (const region of regions) {
        const at = events.indexOf(region)
        expect(at).toBeGreaterThan(openedAt)
        expect(at).toBeLessThan(closedAt)
      }
    }

    const finished = events.filter((e) => e.type === 'run-finished')
    expect(finished).toHaveLength(1)
    expect(finished[0]).toMatchObject({
      runId: handle.runId,
      reason: 'completed',
      pagesQueued: 20,
      pagesCleaned: 20,
      nextPageIndex: null,
    })
    expect(finished[0].regionsCleaned).toBe(
      events.filter((e) => e.type === 'region-done').length,
    )
    expect(events.at(-1)).toMatchObject({ type: 'notice', key: 'notice.run.finished' })
  })

  it('keeps completed regions and stops emitting when cancelled mid-run', async () => {
    const { backend, events } = makeBackend()

    const handle = await begin(backend.runClean({ scope: 'chapter', chapterId: CHAPTER }))
    expect(handle.runId).not.toBeNull()

    // Long enough for a couple of pages, far short of all twenty.
    await vi.advanceTimersByTimeAsync(900)
    const cleanedBefore = events.filter((e) => e.type === 'region-done').length
    expect(cleanedBefore).toBeGreaterThan(0)
    expect(events.some((e) => e.type === 'run-finished')).toBe(false)

    const cancelled = await begin(backend.cancelRun())
    expect(cancelled).toBe(handle.runId)

    const finished = events.filter((e) => e.type === 'run-finished')
    expect(finished).toHaveLength(1)
    expect(finished[0].reason).toBe('cancelled')
    expect(finished[0].pagesCleaned).toBeLessThan(20)
    expect(finished[0].nextPageIndex).not.toBeNull()

    const eventCount = events.length
    await vi.advanceTimersByTimeAsync(60_000)
    expect(events).toHaveLength(eventCount)

    // The regions that finished are still cleaned; the job is incomplete.
    const projects = await settle(backend.listProjects())
    const chapter = projects
      .flatMap((project) => project.chapters)
      .find((c) => c.id === CHAPTER)
    const cleanedRegions = (await pagedIn(backend, chapter, settle))
      .flatMap((page) => page.regions)
      .filter((region) => region.outcome === 'cleaned')
    expect(cleanedRegions).toHaveLength(cleanedBefore)
    expect(chapter.pages.some((page) => page.status === 'unclean')).toBe(true)
    expect(chapter.pages.every((page) => page.status !== 'cleaning')).toBe(true)
  })

  it('restores a region so a deleted mask is addressable again', async () => {
    const { backend } = makeBackend()

    const opened = await settle(
      backend.openChapter({ projectId: 'wandering-moon', chapterId: 'wandering-moon-ch12' }),
    )
    const region = (await pagedIn(backend, opened.chapter, settle))
      .flatMap((page) => page.regions)
      .find((candidate) => candidate.mask)
    const maskId = region.mask.id

    const deleted = await settle(backend.deleteMask({ maskId }))
    // Deleting a mask deletes the row: no region comes back, and none is left
    // on the page for the panel to list as an empty one.
    expect(deleted.region).toBeNull()
    expect(deleted.pageStatus).toBeTypeOf('string')
    const afterDelete = (await pagedIn(backend, opened.chapter, settle)).flatMap((p) => p.regions)
    expect(afterDelete.some((candidate) => candidate.id === region.id)).toBe(false)
    expect(maskRows(afterDelete).some((row) => row.id === region.id)).toBe(false)
    // The undo route: without it the mask would be gone from the backend and
    // every action on the restored row would find nothing.
    expect(await settle(backend.rerunMask({ maskId, kind: 'stronger' }))).toBeNull()

    const restored = await settle(backend.restoreRegion({ regionId: region.id, region }))
    expect(restored.mask.id).toBe(maskId)
    expect(restored.outcome).toBe('cleaned')
    expect(await settle(backend.rerunMask({ maskId, kind: 'stronger' }))).not.toBeNull()
  })

  it('re-runs a mask at the rung it already used, and at a rung it is given', async () => {
    const { backend, events } = makeBackend()

    const opened = await settle(
      backend.openChapter({ projectId: 'wandering-moon', chapterId: 'wandering-moon-ch12' }),
    )
    const region = (await pagedIn(backend, opened.chapter, settle))
      .flatMap((page) => page.regions)
      .find((candidate) => candidate.mask)
    const maskId = region.mask.id
    const engine = region.mask.provenance.engine

    // Try again: the same rung, a new revision. The Layers row's own control,
    // and the one thing the four ladder actions could not express.
    const again = await settle(backend.rerunMask({ maskId, kind: 'retry' }))
    expect(again.mask.provenance.engine).toBe(engine)
    expect(again.mask.sequence).toBeGreaterThan(region.mask.sequence)
    expect(events.some((e) => e.key === 'notice.mask.rerunAgain')).toBe(true)

    // A named rung, whichever direction it lies in.
    const named = await settle(
      backend.rerunMask({ maskId: again.mask.id, kind: 'engine', engine: 'lama' }),
    )
    expect(named.mask.provenance.engine).toBe('lama')
    expect(events.some((e) => e.key === 'notice.mask.rerunEngine')).toBe(true)

    // A rung this build does not have is not a guess: it runs again unchanged.
    const unknown = await settle(
      backend.rerunMask({ maskId: named.mask.id, kind: 'engine', engine: 'diffusion-9000' }),
    )
    expect(unknown.mask.provenance.engine).toBe('lama')

    // The retired Denoise fill, named by an older caller, is read as Fill.
    const legacy = await settle(
      backend.rerunMask({ maskId: unknown.mask.id, kind: 'engine', engine: 'denoise' }),
    )
    expect(legacy.mask.provenance.engine).toBe('fill')
  })

  it('creates a hand-drawn region, and removes it again on undo', async () => {
    const { backend, events } = makeBackend()

    const opened = await settle(
      backend.openChapter({ projectId: 'wandering-moon', chapterId: 'wandering-moon-ch12' }),
    )
    const page = (await pagedIn(backend, opened.chapter, settle))[0]
    const before = page.regions.length
    const bbox = { x: 42, y: 42, w: 12, h: 9 }

    const created = await settle(
      backend.createRegion({
        chapterId: opened.chapter.id,
        pageIndex: page.index,
        bbox,
        tool: 'brush',
        params: { mode: 'add' },
      }),
    )

    // A hand mask is identical in kind to an automatic one; `source` is the
    // only difference, and the geometry is the caller's own.
    expect(created.region.source).toBe('hand')
    expect(created.region.tool).toBe('brush')
    expect(created.region.outcome).toBe('cleaned')
    expect(created.region.bbox).toEqual(bbox)
    expect(created.region.mask.provenance.engine).toBe('fill')
    expect(created.pageStatus).toBeTypeOf('string')

    // A listing carries headers, so the count comes off the header and the
    // regions come from a window - which is the whole of §1b at the seam.
    const headerFor = (projects) =>
      projects
        .flatMap((project) => project.chapters)
        .find((chapter) => chapter.id === opened.chapter.id)
        .pages.find((candidate) => candidate.id === page.id)
    expect(headerFor(await settle(backend.listProjects())).regionCount).toBe(before + 1)

    // And the header carries the other two counts as well, because the Pages
    // list draws a ratio and a ✓ mark for a page it is not holding.
    const header = headerFor(await settle(backend.listProjects()))
    expect(header.resident).toBe(false)
    expect(header.doneCount).toBeGreaterThan(0)
    expect(header.doneCount + header.reviewCount).toBeLessThanOrEqual(header.regionCount)

    // Undo: before the gesture the region was not there, and that is what the
    // snapshot says. Redo puts the same region back, mask and all.
    await settle(backend.restoreRegion({ regionId: created.region.id, region: null }))
    expect(headerFor(await settle(backend.listProjects())).regionCount).toBe(before)

    const redone = await settle(
      backend.restoreRegion({ regionId: created.region.id, region: created.region }),
    )
    expect(redone.mask.id).toBe(created.region.mask.id)
    expect(headerFor(await settle(backend.listProjects())).regionCount).toBe(before + 1)

    // A re-run finds the restored mask, which is the whole point of routing
    // undo back through the seam.
    expect(
      await settle(backend.rerunMask({ maskId: created.region.mask.id, kind: 'simpler' })),
    ).not.toBeNull()

  })

  // **No notice, and no snapping mechanism to report one about.** The tool used
  // to say `aiSnapped` or `aiFallback` - which of the detector's answers made
  // the mask - and there is no detector on this path any more: the stroke is
  // the mask. What the stroke *does* carry is
  // the engine, named outright, and the committed mask has to record it.
  it('cleans an AI mask brush stroke with the engine it named, and says nothing', async () => {
    const { backend, events } = makeBackend()

    const opened = await settle(
      backend.openChapter({ projectId: 'wandering-moon', chapterId: 'wandering-moon-ch12' }),
    )
    const region = (await pagedIn(backend, opened.chapter, settle))
      .flatMap((page) => page.regions)
      .find((candidate) => candidate.detected !== false)

    const applied = await settle(
      backend.applyTool({
        tool: 'aiMaskBrush',
        regionId: region.id,
        params: { engine: 'lama' },
      }),
    )

    expect(applied.mask.provenance.engine).toBe('lama')
    const keys = events.filter((e) => e.type === 'notice').map((e) => e.key)
    expect(keys).not.toContain('notice.tool.aiSnapped')
    expect(keys).not.toContain('notice.tool.aiFallback')
  })

  // Text cleanup page runs use startRun. applyTool edits a stored region only.
  it('does not start a Text cleanup run through applyTool', async () => {
    const { backend } = makeBackend()
    const opened = await settle(backend.openChapter({ projectId: 'tsuki-to-hane', chapterId: CHAPTER }))
    const page = (await pagedIn(backend, opened.chapter, settle)).find((candidate) => candidate.status === 'unclean')
    const region = page.regions.find((candidate) => candidate.outcome !== 'detected')

    const edit = await settle(backend.applyTool({
      tool: 'autoClean', regionId: region.id, chapterId: opened.chapter.id, pageIndex: page.index,
      params: { scope: 'project', mode: 'detect' },
    }))
    expect(edit.status).not.toBe('run-started')
    expect(edit.runId).toBeUndefined()

    const absent = await settle(backend.applyTool({
      tool: 'autoClean', chapterId: opened.chapter.id, pageIndex: page.index, params: { mode: 'detect' },
    }))
    expect(absent.status).toBe('not-found')
    expect(absent.runId).toBeUndefined()
  })

  it('keeps a region a hand tool has touched on the detected side of the fork', async () => {
    const { backend, events } = makeBackend()

    const opened = await settle(
      backend.openChapter({ projectId: 'wandering-moon', chapterId: 'wandering-moon-ch12' }),
    )
    const missed = (await pagedIn(backend, opened.chapter, settle))
      .flatMap((page) => page.regions)
      .find((region) => region.detected === false)

    // A hand tool marks the region detected, deliberately: after it the app
    // holds a box for text the auto pass never found, which is what puts it
    // back inside the automatic queue if the mask is later deleted.
    await settle(backend.applyTool({ tool: 'aiMaskBrush', regionId: missed.id, params: {} }))
    events.length = 0
    const again = await settle(
      backend.applyTool({ tool: 'aiMaskBrush', regionId: missed.id, params: {} }),
    )

    expect(again.region.detected).toBe(true)
    const keys = events.filter((e) => e.type === 'notice').map((e) => e.key)
    expect(keys).not.toContain('notice.tool.aiSnapped')
  })

  it('adopts a hand-drawn geometry and reports the page it moved', async () => {
    const { backend } = makeBackend()

    const opened = await settle(
      backend.openChapter({ projectId: 'tsuki-to-hane', chapterId: CHAPTER }),
    )
    const page = (await pagedIn(backend, opened.chapter, settle)).find(
      (candidate) => candidate.status === 'unclean',
    )
    const region = page.regions[0]
    const bbox = { x: 11, y: 12, w: 13, h: 14 }

    const result = await settle(
      backend.applyTool({
        tool: 'shapes',
        regionId: region.id,
        chapterId: opened.chapter.id,
        pageIndex: page.index,
        params: { bbox },
      }),
    )

    // A hand-drawn mask is authoritative: the geometry replaces the
    // detector's box rather than being reconciled with it.
    expect(result.status).toBe('applied')
    expect(result.region.bbox).toEqual(bbox)
    expect(result.region.source).toBe('hand')
    // And the page's status comes back with it, as it does from every other
    // region-level edit.
    expect(result.pageStatus).toBe('cleaned')
  })

  // `engineCeiling` is a ceiling in both directions. `startRun` pins every run
  // to the highest *local* rung so a batch can never reach the cloud, and that
  // pin must not become a way of reaching past a user whose setting is lower:
  // the stored ceiling here is `lama`, one rung below the pin.
  it('never routes past the stored ceiling, whatever the caller asks for', async () => {
    const { backend, events } = makeBackend()
    await settle(backend.runClean({ scope: 'chapter', chapterId: CHAPTER, engineCeiling: 'cloud' }))

    const engines = new Set(
      events
        .filter((event) => event.type === 'region-done' && event.region.mask)
        .map((event) => event.region.mask.provenance.engine),
    )
    expect(engines.size).toBeGreaterThan(0)
    expect(engines.has('cloud')).toBe(false)
  })

  it('honors bubbleEngine and outsideEngine picks during auto clean', async () => {
    const { backend: fillBackend, events: fillEvents } = makeBackend()
    await settle(
      fillBackend.runClean({
        scope: 'chapter',
        chapterId: CHAPTER,
        bubbleEngine: 'fill',
        outsideEngine: 'redraw',
      }),
    )
    const bubbleFill = fillEvents
      .filter((e) => e.type === 'region-done' && e.region.kind === 'bubble')
      .map((e) => e.region.mask.provenance.engine)
    const outsideRedraw = fillEvents
      .filter((e) => e.type === 'region-done' && e.region.kind !== 'bubble')
      .map((e) => e.region.mask.provenance.engine)
    expect(bubbleFill.length).toBeGreaterThan(0)
    expect(outsideRedraw.length).toBeGreaterThan(0)
    expect(bubbleFill.every((eng) => eng === 'fill')).toBe(true)
    expect(outsideRedraw.every((eng) => eng === 'lama')).toBe(true)

    const { backend: redrawBackend, events: redrawEvents } = makeBackend()
    await settle(
      redrawBackend.runClean({
        scope: 'chapter',
        chapterId: CHAPTER,
        bubbleEngine: 'redraw',
        outsideEngine: 'fill',
      }),
    )
    const bubbleRedraw = redrawEvents
      .filter((e) => e.type === 'region-done' && e.region.kind === 'bubble')
      .map((e) => e.region.mask.provenance.engine)
    const outsideFill = redrawEvents
      .filter((e) => e.type === 'region-done' && e.region.kind !== 'bubble')
      .map((e) => e.region.mask.provenance.engine)
    expect(bubbleRedraw.length).toBeGreaterThan(0)
    expect(outsideFill.length).toBeGreaterThan(0)
    expect(bubbleRedraw.every((eng) => eng === 'lama')).toBe(true)
    expect(outsideFill.every((eng) => eng === 'fill')).toBe(true)

    const { backend: colorBackend, events: colorEvents } = makeBackend()
    await settle(
      colorBackend.runClean({
        scope: 'chapter',
        chapterId: CHAPTER,
        bubbleEngine: 'fill',
        bubbleColor: '#ffffff',
      }),
    )
    expect(colorEvents.filter((e) => e.type === 'region-done').length).toBeGreaterThan(0)
  })

  it('cleans gate-skipped outside-bubble regions only when the run opts in', async () => {
    // Wandering Moon Ch. 12 carries the fixture's one "text outside a speech
    // bubble" region, on a page the auto pass has otherwise finished.
    const WANDERING = 'wandering-moon-ch12'
    const outsideHeld = (region) =>
      region.outcome === 'gate-skipped' && region.gateSkipCause === 'outside-bubble'
    const regionsOf = async (backend) => {
      const projects = await settle(backend.listProjects())
      const chapter = projects.flatMap((p) => p.chapters).find((c) => c.id === WANDERING)
      return (await pagedIn(backend, chapter, settle)).flatMap((page) => page.regions)
    }

    // Default: the region stays held, and no region-done event names it.
    const { backend: held, events: heldEvents } = makeBackend()
    await settle(held.runClean({ scope: 'chapter', chapterId: WANDERING, outsideEngine: 'lama' }))
    expect(
      heldEvents.filter((e) => e.type === 'region-done' && e.region.gateSkipCause === 'outside-bubble'),
    ).toEqual([])
    const stillHeld = (await regionsOf(held)).filter(outsideHeld)
    expect(stillHeld.length).toBeGreaterThan(0)

    // Opted in: the same region is cleaned, on the outside-text engine, and
    // is no longer a gate skip.
    const { backend: opted, events: optedEvents } = makeBackend()
    await settle(
      opted.runClean({
        scope: 'chapter',
        chapterId: WANDERING,
        outsideEngine: 'lama',
        outsideBubbles: 'clean',
      }),
    )
    const after = await regionsOf(opted)
    expect(after.filter(outsideHeld)).toEqual([])
    const cleanedOutside = optedEvents
      .filter((e) => e.type === 'region-done' && e.region.kind === 'outside')
      .map((e) => e.region.mask.provenance.engine)
    expect(cleanedOutside.length).toBeGreaterThanOrEqual(stillHeld.length)
    expect(cleanedOutside.every((engine) => engine === 'lama')).toBe(true)
  })

  it('reports the empty result when a chapter has no regions at all', async () => {
    const { backend, events } = makeBackend()

    await settle(backend.runClean({ scope: 'chapter', chapterId: EMPTY_CHAPTER }))

    expect(events.filter((e) => e.type === 'region-done')).toHaveLength(0)
    expect(events.filter((e) => e.type === 'page-done')).toHaveLength(48)

    const notices = events.filter((e) => e.type === 'notice')
    expect(notices).toHaveLength(1)
    expect(notices[0]).toMatchObject({
      key: 'notice.chapter.emptyResult',
      params: { regions: 0, pages: 48 },
      tone: 'warn',
    })
    expect(notices.some((e) => e.key === 'notice.run.finished')).toBe(false)
  })
})

/** The width and height a PNG data URL declares, after checking that its image data inflates to them. */
function pngSize(dataUrl) {
  const bytes = Buffer.from(String(dataUrl).replace(/^data:image\/png;base64,/, ''), 'base64')
  expect([...bytes.subarray(0, 8)]).toEqual([137, 80, 78, 71, 13, 10, 26, 10])
  const width = bytes.readUInt32BE(16)
  const height = bytes.readUInt32BE(20)
  const depth = bytes[24]
  const color = bytes[25]
  const channels = { 0: 1, 2: 3, 6: 4 }[color]
  const idat = []
  for (let at = 8; at < bytes.length;) {
    const length = bytes.readUInt32BE(at)
    const type = bytes.toString('latin1', at + 4, at + 8)
    if (type === 'IDAT') idat.push(bytes.subarray(at + 8, at + 8 + length))
    at += 12 + length
  }
  expect(depth).toBe(8)
  expect(inflateSync(Buffer.concat(idat)).length).toBe((width * channels + 1) * height)
  return { width, height }
}

const TIMING_CLOUD = 900

describe('remote analysis mock', () => {
  async function configured(scenario) {
    const backend = createMockBackend({ timing: { ...TIMING, cloud: TIMING_CLOUD }, remoteAnalysisScenario: scenario })
    await settle(backend.writeSettings({ cloudEngines: 'allowed' }))
    await settle(backend.writeInferenceConfig({ config: {
      schemaVersion: 1, selectedTarget: { type: 'modal', profile_id: 'm1' }, beamProfiles: {},
      modalProfiles: { m1: { id: 'm1', name: 'Modal test', endpointUrl: 'https://worker.modal.run/mc/v1',
        canonicalOrigin: 'https://worker.modal.run', canonicalOriginFingerprint: 'a'.repeat(64),
        createdAtMs: 1, updatedAtMs: 1 } },
    } }))
    return backend
  }

  it('discloses one page and returns review-only evidence after explicit acknowledgements', async () => {
    const backend = await configured()
    const records = []
    backend.onRemoteAnalysis((record) => records.push(record))
    const proposal = await settle(backend.proposeRemoteAnalysis({ chapterId: CHAPTER, pageIndex: 0,
      provider: 'modal', profileId: 'm1', capability: 'text_mask_sam_ts@1' }))
    const proposed = await backend.getRemoteAnalysisStatus({ proposalId: proposal.proposalId })
    expect(proposed).toMatchObject({ created_at_ms: proposal.issuedAtMs, tile_submission_started: false })
    expect(records).toEqual([])
    expect(proposal.pages).toBe(1)
    expect(proposal.includesSurroundingArt).toBe(true)
    expect(proposal.costEstimateUsd).toBeNull()
    expect(proposal.tiles.length).toBeGreaterThan(0)
    await expect(backend.confirmRemoteAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: false, retentionAcknowledged: true })).rejects.toThrow('rights_attestation_required')
    const result = await settle(backend.confirmRemoteAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true }))
    expect(result.samWriteEligible).toBe(false)
    expect(result.remoteSource).toContain('remote:modal:text_mask_sam_ts@1')
    expect(await backend.getRemoteAnalysisStatus({ proposalId: proposal.proposalId })).toMatchObject({
      phase: { phase: 'attached_evidence' }, tile_submission_started: true,
    })
    // Review-only evidence of a drawn page: a source image, a mask and components, never a write.
    expect(result.samBackend).toBe('remote')
    // Provenance names the 1.1.0 plan, as the native grouping records it.
    expect(result.evidence.models).toEqual([{ model: 'samTsL', execution: 'cloud', spatial: CLOUD_ANALYSIS_SPATIAL }])
    expect(result.evidence.components.length).toBeGreaterThan(0)
    expect(pngSize(result.sourceDataUrl)).toEqual({ width: proposal.pageWidth, height: proposal.pageHeight })
    expect(pngSize(result.maskDataUrl)).toEqual({ width: proposal.pageWidth, height: proposal.pageHeight })
    await expect(settle(backend.prepareComponentWrite({ analysisId: result.analysisId, chapterId: CHAPTER, pageIndex: 0,
      componentId: 'sam-00001', allowOutsideBubbles: true }))).rejects.toThrow('Remote analysis is review-only')
    await expect(settle(backend.loadComponentCorrection({ analysisId: result.analysisId, componentId: 'sam-00001' })))
      .rejects.toThrow('Remote analysis is review-only')
  })

  it('returns detector regions only for the RT capability', async () => {
    const backend = await configured()
    const proposal = await settle(backend.proposeRemoteAnalysis({ chapterId: CHAPTER, pageIndex: 0,
      provider: 'modal', profileId: 'm1', capability: 'text_regions_rt@1' }))
    const result = await settle(backend.confirmRemoteAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true }))
    expect(result).toMatchObject({ rtBackend: 'remote', samBackend: null, maskDataUrl: null, samWriteEligible: false })
    expect(result.evidence.components).toEqual([])
    expect(result.evidence.regions.length).toBeGreaterThan(0)
    expect(result.evidence.regions.every((region) => region.detectorOnly)).toBe(true)
    expect(result.evidence.models).toEqual([{ model: 'ogkaluFull', execution: 'cloud', spatial: CLOUD_ANALYSIS_SPATIAL }])
  })

  it('refuses a longstrip chapter before anything is proposed', async () => {
    const backend = await configured()
    await expect(settle(backend.proposeRemoteAnalysis({ chapterId: 'neon-alley-ch4', pageIndex: 0,
      provider: 'modal', profileId: 'm1', capability: 'text_mask_sam_ts@1' })))
      .rejects.toThrow('Remote analysis currently requires a paginated chapter')
  })

  it('leaves a tile in the unknown state, with the cancel request recorded, when the connection drops', async () => {
    const backend = await configured('unknown')
    const records = []
    backend.onRemoteAnalysis((record) => records.push(record))
    const proposal = await settle(backend.proposeRemoteAnalysis({ chapterId: CHAPTER, pageIndex: 0,
      provider: 'modal', profileId: 'm1', capability: 'text_mask_sam_ts@1' }))
    expect(proposal.tiles.length).toBeGreaterThan(1)
    const running = backend.confirmRemoteAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true })
    const observed = running.catch((error) => error)
    // The first tile answers; cancel lands while the second is out.
    await vi.advanceTimersByTimeAsync(TIMING_CLOUD + 1)
    expect(records.at(-1).phase).toEqual({ phase: 'submitted_tile', index: 1 })
    expect(records.at(-1).completed_tiles).toBe(1)
    expect(await backend.cancelRemoteAnalysis({ proposalId: proposal.proposalId })).toBe(true)
    await vi.runAllTimersAsync()
    expect(String((await observed).message)).toContain('transport error')
    const status = await backend.getRemoteAnalysisStatus({ proposalId: proposal.proposalId })
    expect(status.phase).toEqual({ phase: 'unknown_remote_state', index: 1 })
    expect(status.cancel_requested).toBe(true)
    expect(records.at(-1)).toEqual(status)
  })

  it('supports cancel, stale results, and a missing-capability refusal', async () => {
    const backend = await configured()
    const spec = { chapterId: CHAPTER, pageIndex: 0, provider: 'modal', profileId: 'm1', capability: 'text_mask_sam_ts@1' }
    const proposal = await settle(backend.proposeRemoteAnalysis(spec))
    const running = backend.confirmRemoteAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true })
    await vi.advanceTimersByTimeAsync(0)
    expect(await backend.cancelRemoteAnalysis({ proposalId: proposal.proposalId })).toBe(true)
    await expect(settle(running)).rejects.toThrow('analysis_cancelled')
    expect((await backend.getRemoteAnalysisStatus({ proposalId: proposal.proposalId })).phase.phase).toBe('cancelled')
    const stale = await configured('stale')
    const staleProposal = await settle(stale.proposeRemoteAnalysis(spec))
    await expect(settle(stale.confirmRemoteAnalysis({ proposalId: staleProposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true }))).rejects.toThrow('analysis_stale')
    expect((await stale.getRemoteAnalysisStatus({ proposalId: staleProposal.proposalId })).completed_tiles).toBe(0)
    const missing = await configured('missingCapability')
    await expect(settle(missing.proposeRemoteAnalysis(spec))).rejects.toThrow('capability_unavailable')
  })

  it('runs a companion model in the same review, and fuses both answers', async () => {
    const backend = await configured()
    const proposal = await settle(backend.proposeRemoteAnalysis({ chapterId: CHAPTER, pageIndex: 0,
      provider: 'modal', profileId: 'm1', capability: 'text_mask_sam_ts@1', companion: 'text_regions_rt@1' }))
    expect(proposal.models.map((model) => model.capability)).toEqual(['text_mask_sam_ts@1', 'text_regions_rt@1'])
    const result = await settle(backend.confirmRemoteAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true }))
    expect(result).toMatchObject({ rtBackend: 'remote', samBackend: 'remote', samWriteEligible: false })
    expect(result.remoteSource).toBe('remote:modal:text_mask_sam_ts@1+text_regions_rt@1')
    await expect(settle(backend.proposeRemoteAnalysis({ chapterId: CHAPTER, pageIndex: 0, provider: 'modal',
      profileId: 'm1', capability: 'text_mask_sam_ts@1', companion: 'text_mask_sam_ts@1' })))
      .rejects.toThrow('capability_unavailable')
  })
})

describe('Auto clean with cloud detection, in the mock', () => {
  const CLOUD = { rtFull: 'local', samTs: 'cloud' }
  const run = { scope: 'page', chapterId: CHAPTER, pageIndex: 0, detectorModels: ['samTs'], analysisTargets: CLOUD }

  async function configured(scenario) {
    const backend = createMockBackend({ timing: { ...TIMING, cloud: TIMING_CLOUD }, remoteAnalysisScenario: scenario })
    await settle(backend.writeSettings({ cloudEngines: 'allowed' }))
    await settle(backend.writeInferenceConfig({ config: {
      schemaVersion: 1, selectedTarget: { type: 'modal', profile_id: 'm1' }, beamProfiles: {},
      modalProfiles: { m1: { id: 'm1', name: 'Modal test', endpointUrl: 'https://worker.modal.run/mc/v1',
        canonicalOrigin: 'https://worker.modal.run', canonicalOriginFingerprint: 'a'.repeat(64),
        createdAtMs: 1, updatedAtMs: 1 } },
    } }))
    return backend
  }

  async function granted(backend, pageIndices = [0]) {
    const proposal = await settle(backend.proposeRunAnalysis({ chapterId: CHAPTER, pageIndices,
      capabilities: ['text_mask_sam_ts@1'], provider: 'modal', profileId: 'm1' }))
    return settle(backend.confirmRunAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true }))
  }

  it('refuses a run that routes a stage to the cloud without a grant', async () => {
    const backend = await configured()
    await expect(settle(backend.runClean(run))).rejects.toThrow('cloud_run_grant_required')
    await expect(settle(backend.runClean({ ...run, scope: 'project' }))).rejects.toThrow('cloud_run_scope_unsupported')
    await expect(settle(backend.runClean({ ...run, analysisTargets: { rtFull: 'local', samTs: 'local' },
      cloudGrant: 'stray' }))).rejects.toThrow('cloud_run_grant_mismatch: capabilities')
  })

  it('discloses the pages and models, and spends its grant once', async () => {
    const backend = await configured()
    const proposal = await settle(backend.proposeRunAnalysis({ chapterId: CHAPTER, pageIndices: [0],
      capabilities: ['text_mask_sam_ts@1'], provider: 'modal', profileId: 'm1' }))
    expect(proposal).toMatchObject({ pages: 1, costEstimateUsd: null, includesSurroundingArt: true,
      capabilities: ['text_mask_sam_ts@1'] })
    expect(proposal.totalTiles).toBeGreaterThan(0)
    await expect(backend.confirmRunAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: false })).rejects.toThrow('retention_acknowledgement_required')
    const first = await settle(backend.confirmRunAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true }))
    expect(first.pageIndices).toEqual([0])
    // A proposal answers one confirm.
    await expect(backend.confirmRunAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true })).rejects.toThrow('analysis_proposal_missing')
    const grant = await granted(backend)
    const handle = await begin(backend.runClean({ ...run, cloudGrant: grant.grantId }))
    expect(handle.runId).toBeTruthy()
    await vi.runAllTimersAsync()
    await expect(settle(backend.runClean({ ...run, cloudGrant: grant.grantId }))).rejects.toThrow('cloud_run_grant_missing')
  })

  it('holds a grant to its pages and its chapter', async () => {
    const backend = await configured()
    const grant = await granted(backend)
    await expect(settle(backend.runClean({ ...run, pageIndex: 1, cloudGrant: grant.grantId })))
      .rejects.toThrow('cloud_run_grant_mismatch: pages')
    // A long strip is proposed page by page, like any chapter.
    const longstrip = await settle(backend.proposeRunAnalysis({ chapterId: 'neon-alley-ch4', pageIndices: [0],
      capabilities: ['text_mask_sam_ts@1'], provider: 'modal', profileId: 'm1' }))
    expect(longstrip.pageIndices).toEqual([0])
  })

  it('proposes a chapter as the pages its run would walk, and one grant starts that run', async () => {
    const backend = await configured()
    const proposal = await settle(backend.proposeRunAnalysis({ chapterId: CHAPTER, scope: 'chapter',
      capabilities: ['text_mask_sam_ts@1'], provider: 'modal', profileId: 'm1' }))
    expect(proposal.pages).toBe(20)
    expect(proposal.pageIndices).toEqual([...Array(20).keys()])
    const grant = await settle(backend.confirmRunAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true }))
    // A grant for one page does not start the chapter.
    const page = await granted(backend)
    await expect(settle(backend.runClean({ ...run, scope: 'chapter', cloudGrant: page.grantId })))
      .rejects.toThrow('cloud_run_grant_mismatch: pages')
    const handle = await begin(backend.runClean({ ...run, scope: 'chapter', cloudGrant: grant.grantId }))
    expect(handle.runId).toBeTruthy()
    expect(handle.pages).toHaveLength(20)
    await vi.runAllTimersAsync()
    await expect(settle(backend.proposeRunAnalysis({ chapterId: CHAPTER, scope: 'project',
      capabilities: ['text_mask_sam_ts@1'], provider: 'modal', profileId: 'm1' }))).rejects.toThrow('cloud_run_scope_unsupported')
  })

  it('reports a failed cloud page instead of cleaning it locally', async () => {
    const events = []
    const backend = await configured('runFail')
    backend.subscribe((event) => events.push(event))
    const grant = await granted(backend)
    await expect(settle(backend.runClean({ ...run, cloudGrant: grant.grantId }))).rejects.toThrow('cloud_analysis_failed')
    expect(events.some((event) => event.type === 'notice' && event.key === 'cloud.analysis.run.pageFailed')).toBe(true)
  })

  // What the interface sends for Detect on: Cloud GPU (`pipelines.js#runDetection`):
  // CTD and the reader here beside both cloud stages. Only Small is refused.
  it('takes the cloud combination with CTD and the reader, refuses Small, and says a missing reader', async () => {
    const BOTH = ['text_mask_sam_ts@1', 'text_regions_rt@1']
    const events = []
    const backend = await configured()
    backend.subscribe((event) => events.push(event))
    const best = { scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'detect', detectorModels: ['ctd', 'rtFull', 'samTs'],
      ocrRescue: true, analysisTargets: { rtFull: 'cloud', samTs: 'cloud' } }
    const proposal = await settle(backend.proposeRunAnalysis({ chapterId: CHAPTER, pageIndices: [0],
      capabilities: BOTH, provider: 'modal', profileId: 'm1' }))
    const grant = await settle(backend.confirmRunAnalysis({ proposalId: proposal.proposalId,
      rightsAttested: true, retentionAcknowledged: true }))
    await expect(settle(backend.runClean({ ...best, detectorModels: ['ctd', 'rtSmall', 'samTs'], cloudGrant: grant.grantId })))
      .rejects.toThrow('cloud_detect_small_unsupported')
    const handle = await begin(backend.runClean({ ...best, cloudGrant: grant.grantId }))
    expect(handle.runId).toBeTruthy()
    // The reader is not installed in a fresh mock: the run says so rather than dropping it.
    const reader = events.filter((event) => event.type === 'notice' && event.key === 'notice.run.ocrRescueUnavailable')
    expect(reader).toHaveLength(1)
    expect(reader[0].params.reason).toContain('hayai-ocr-vision.onnx')
    await vi.runAllTimersAsync()
  })
})

/**
 * `exportChapter` refuses nine different things here and used to refuse one.
 * The mock is the only backend outside a Tauri window, so a refusal it does
 * not produce is a refusal the interface is never seen handling.
 * `src-tauri/src/exporting.rs` is the side that decides them; these assert the
 * mock decides the same ones, in the same shape, and names each one's own
 * reason rather than the overwrite refusal it grew out of. The two it cannot
 * decide - a PSD of a page PSD cannot carry - need per-source mode and depth
 * the mock's pages do not have.
 */
describe('export refusals', () => {
  /** A paginated chapter, and a longstrip one, both from the fixture library. */
  const LONGSTRIP = 'neon-alley-ch4'

  /** The refusal, and the notice it announced, from one call. */
  async function exportWith(spec) {
    const { backend, events } = makeBackend()
    const result = await settle(backend.exportChapter({ chapterId: CHAPTER, ...spec }))
    const notices = events.filter((e) => e.type === 'notice')
    return { result, notices }
  }

  it('names the format it cannot write rather than substituting one', async () => {
    const cases = [
      ['JPEG', 'notice.export.refusedLossyFormat'],
      ['jpg', 'notice.export.refusedLossyFormat'],
      ['WebP', 'notice.export.refusedLossyFormat'],
      ['PSB', 'notice.export.refusedLayeredFormat'],
      ['AVIF', 'notice.export.refusedUnknownFormat'],
    ]
    for (const [format, reasonKey] of cases) {
      const { result, notices } = await exportWith({ format })
      expect(result, format).toMatchObject({ status: 'refused', reasonKey })
      expect(result.fileCount, format).toBeUndefined()
      // The defect this replaced: a JPEG that came back as a PNG, reported as
      // a success. Nothing about this answer says the export happened.
      expect(notices.map((e) => e.key), format).toEqual([reasonKey])
      expect(notices[0].params.format, format).toBe(format)
    }
  })

  it('writes PNG, TIFF, PSD and CBZ, and puts the archive in a file of its own', async () => {
    for (const format of ['PNG', 'TIFF', 'PSD']) {
      const { result } = await exportWith({ format })
      expect(result, format).toMatchObject({ status: 'exported' })
      expect(result.path.endsWith('.cbz'), format).toBe(false)
    }
    const { result } = await exportWith({ format: 'CBZ' })
    expect(result.status).toBe('exported')
    expect(result.path.endsWith('.cbz')).toBe(true)
    expect(result.fileCount).toBeGreaterThan(0)
  })

  it('writes separate masks for a raster page or a PSD, and refuses them inside an archive', async () => {
    for (const format of ['PNG', 'TIFF', 'PSD']) {
      const { result } = await exportWith({ format, masks: 'separate-layer' })
      expect(result, format).toMatchObject({ status: 'exported' })
    }
    const { result, notices } = await exportWith({ format: 'CBZ', masks: 'separate-layer' })
    expect(result).toMatchObject({
      status: 'refused',
      reasonKey: 'notice.export.refusedMaskLayers',
    })
    expect(notices.map((e) => e.key)).toEqual(['notice.export.refusedMaskLayers'])
  })

  it('refuses to stitch a chapter into one PSD', async () => {
    const { backend } = makeBackend()
    const result = await settle(
      backend.exportChapter({ chapterId: LONGSTRIP, format: 'PSD', layout: 'stitched' }),
    )
    expect(result).toMatchObject({
      status: 'refused',
      reasonKey: 'notice.export.refusedStitchedLayered',
    })
  })

  it('takes an absolute destination and refuses a relative one', async () => {
    const chosen = await exportWith({ destination: '/Volumes/scratch/ch107' })
    expect(chosen.result.status).toBe('exported')
    expect(chosen.result.path).toBe('/Volumes/scratch/ch107')

    const relative = await exportWith({ destination: '../up/one' })
    expect(relative.result).toMatchObject({
      status: 'refused',
      reasonKey: 'notice.export.refusedDestination',
    })
  })

  it('still refuses the source folder, and says which folder', async () => {
    const { result, notices } = await exportWith({ destination: 'source-folder' })
    expect(result).toMatchObject({
      status: 'refused',
      reasonKey: 'notice.export.refusedOverwrite',
    })
    expect(notices[0].params.path).toBeTruthy()
  })

  it('stitches a longstrip chapter into one file and counts its gutter', async () => {
    const { backend, events } = makeBackend()
    const result = await settle(
      backend.exportChapter({ chapterId: LONGSTRIP, format: 'PNG', layout: 'stitched' }),
    )
    expect(result.status).toBe('exported')
    expect(result.fileCount).toBe(1)
    expect(result.path.endsWith('.png')).toBe(true)
    // The count is reported, and zero is a real answer - every
    // fixture page is the full width of the strip.
    expect(result.gutterPixels).toBe(0)
    const notices = events.filter((e) => e.type === 'notice')
    expect(notices.map((e) => e.key)).toEqual(['notice.export.stitched'])
    expect(notices[0].params.count).toBe(0)
  })

  it('refuses to stitch a paginated chapter, or to stitch into an archive', async () => {
    const paginated = await exportWith({ layout: 'stitched' })
    expect(paginated.result).toMatchObject({
      status: 'refused',
      reasonKey: 'notice.export.refusedStitchPaginated',
    })

    const { backend } = makeBackend()
    const archive = await settle(
      backend.exportChapter({ chapterId: LONGSTRIP, format: 'CBZ', layout: 'stitched' }),
    )
    expect(archive).toMatchObject({
      status: 'refused',
      reasonKey: 'notice.export.refusedStitchedArchive',
    })
  })
})

describe('cleanAnyway', () => {
  it('cleans a skipped region with the specified engine and defaults to fill', async () => {
    const { backend } = makeBackend()
    const opened = await settle(
      backend.openChapter({ projectId: 'tsuki-to-hane', chapterId: CHAPTER }),
    )
    const loaded = await pagedIn(backend, opened.chapter, settle)
    const page = loaded[0]
    const region = page.regions[0]

    const resultWithEngine = await settle(
      backend.cleanAnyway({ regionId: region.id, engine: 'lama' }),
    )
    expect(resultWithEngine).not.toBeNull()
    expect(resultWithEngine.mask.provenance.engine).toBe('lama')
    expect(resultWithEngine.region.outcome).toBe('cleaned')

    const region2 = page.regions[1]
    const resultDefault = await settle(backend.cleanAnyway({ regionId: region2.id }))
    expect(resultDefault).not.toBeNull()
    expect(resultDefault.mask.provenance.engine).toBe('fill')
    expect(resultDefault.region.outcome).toBe('cleaned')
  })
})

describe('paint and clone engine integration in mock backend', () => {
  it('creates a region with engine paint when brush mode is paint', async () => {
    const { backend } = makeBackend()
    const opened = await settle(
      backend.openChapter({ projectId: 'tsuki-to-hane', chapterId: CHAPTER }),
    )
    const result = await settle(
      backend.createRegion({
        chapterId: opened.chapter.id,
        pageIndex: 0,
        bbox: { x: 10, y: 10, w: 20, h: 20 },
        tool: 'brush',
        params: {
          mode: 'paint',
          color: '#123456',
          opacity: 100,
          flow: 100,
          stroke: { points: [{ x: 10, y: 10 }], radius: 10 },
          paint: {
            points: [{ x: 10, y: 10, p: 0.5 }],
            color: '#123456',
            opacity: 100,
            flow: 100,
            hardness: 70,
            spacing: 12,
            pressureSize: true,
            pressureOpacity: false,
            seed: 42,
          },
        },
      }),
    )
    expect(result).not.toBeNull()
    expect(result.region.tool).toBe('brush')
    expect(result.region.mask?.provenance.engine).toBe('paint')
  })

  it('creates a region with engine clone for cloneHeal tool', async () => {
    const { backend } = makeBackend()
    const opened = await settle(
      backend.openChapter({ projectId: 'tsuki-to-hane', chapterId: CHAPTER }),
    )
    const result = await settle(
      backend.createRegion({
        chapterId: opened.chapter.id,
        pageIndex: 0,
        bbox: { x: 10, y: 10, w: 20, h: 20 },
        tool: 'cloneHeal',
        params: {
          mode: 'heal',
          alignment: 'aligned',
          opacity: 90,
          flow: 80,
          stroke: { points: [{ x: 10, y: 10 }], radius: 10 },
          cloneSource: { x: 50, y: 50 },
          cloneOffset: { x: 40, y: 40 },
          paint: {
            points: [{ x: 10, y: 10, p: 0.5 }],
            seed: 99,
          },
        },
      }),
    )
    expect(result).not.toBeNull()
    expect(result.region.tool).toBe('cloneHeal')
    expect(result.region.mask?.provenance.engine).toBe('clone')
  })
})

/**
 * A layer's style through the mock seam, by the same rules the native
 * `set_layer_style` enforces: a paint stroke moves and turns, a clone blend
 * is fixed where it was made and keeps only its opacity. A refusal names its
 * catalogue key and changes nothing. (A detection's refusal is pinned in
 * `MaskRow.dom.test.js`, which has a detect run to hand.)
 */
describe('layer style in the mock backend', () => {
  /** @param {any} backend @param {string} tool @param {object} params */
  async function stroke(backend, tool, params) {
    const opened = await settle(backend.openChapter({ projectId: 'tsuki-to-hane', chapterId: CHAPTER }))
    const result = await settle(
      backend.createRegion({
        chapterId: opened.chapter.id,
        pageIndex: 0,
        bbox: { x: 10, y: 10, w: 20, h: 20 },
        tool,
        params: { stroke: { points: [{ x: 10, y: 10 }], radius: 10 }, paint: { points: [{ x: 10, y: 10, p: 0.5 }], seed: 7 }, ...params },
      }),
    )
    return { opened, region: result.region }
  }

  it('moves and turns a paint stroke, keeping the box it was made in', async () => {
    const { backend } = makeBackend()
    const { region } = await stroke(backend, 'brush', { mode: 'paint', color: '#123456', opacity: 100, flow: 100 })
    const moved = await settle(
      backend.setLayerStyle({ regionId: region.id, layer: { opacity: 60, offsetX: 160, offsetY: 0, rotation: 0, locked: false } }),
    )
    expect(moved.mask.layer).toMatchObject({ opacity: 60, offsetX: 160, rotation: 0 })
    expect(moved.mask.sourceBbox).toEqual(region.bbox)
    expect(moved.bbox.x).toBeGreaterThan(region.bbox.x)
    expect(moved.bbox.w).toBeCloseTo(region.bbox.w)
    const locked = await settle(backend.setLayerStyle({ regionId: region.id, layer: { ...moved.mask.layer, locked: true } }))
    await expect(settle(backend.setLayerStyle({ regionId: region.id, layer: { ...locked.mask.layer, offsetX: 0 } })))
      .rejects.toThrow('masks.refused.locked')
  })

  it('keeps a clone blend where it was made, and still fades it', async () => {
    const { backend } = makeBackend()
    const { region } = await stroke(backend, 'cloneHeal', {
      mode: 'heal', alignment: 'aligned', opacity: 90, flow: 80, cloneSource: { x: 50, y: 50 }, cloneOffset: { x: 40, y: 40 },
    })
    await expect(settle(backend.setLayerStyle({ regionId: region.id, layer: { offsetX: 5 } })))
      .rejects.toThrow('masks.refused.fixed')
    await expect(settle(backend.setLayerStyle({ regionId: region.id, layer: { locked: true } })))
      .rejects.toThrow('masks.refused.noLock')
    const faded = await settle(backend.setLayerStyle({ regionId: region.id, layer: { opacity: 30 } }))
    expect(faded.mask.layer).toMatchObject({ opacity: 30, offsetX: 0, offsetY: 0, rotation: 0 })
    expect(faded.bbox).toEqual(region.bbox)
  })
})

/**
 * The model catalogue. What is asserted is the
 * *shape* the interface reads and the one rule the mock exists to make
 * visible in a browser - one artefact missing, so the engine gating is
 * something a person can see rather than something only a Tauri window with a
 * half-empty models directory ever shows.
 */
describe('the model catalogue', () => {
  it('reports every artefact, with exactly one of them not installed', async () => {
    const { backend } = makeBackend()
    const view = await settle(backend.listModels())

    expect(view.models).toHaveLength(13)
    for (const model of view.models) {
      expect(model.kindKey).toMatch(/^models\.kind\./)
      expect(model.bytes).toBeGreaterThan(0)
      expect(model.sha256).toMatch(/^[a-f0-9]{64}$/)
      expect(Array.isArray(model.requiredBy)).toBe(true)
    }
    // The redraw model, which is what makes the engine gating visible, and the
    // two optional readers' parts, which gate nothing and are absent because
    // that is what a machine that has not chosen to fetch them looks like,
    // and the optional local page denoise pair for the same reason.
    const missing = view.models.filter((model) => !model.installed).map((model) => model.id)
    expect(missing).toEqual(['inpainter', 'ocrEncoder', 'ocrDecoder', 'ocrVocab', 'hayaiVision', 'hayaiDecoder', 'hayaiTokenizer', 'pageDenoiseModel', 'pageDenoiseSeams'])
    expect(view.runtime.installed).toBe(true)
    expect(view.modelsDir).toBeTruthy()
    // The token never comes back across the seam - only whether one is stored
    // and where it is kept.
    expect(view).not.toHaveProperty('hfToken')
    expect(view.hasToken).toBe(false)
    expect(view.tokenStore).toBe('fileNoStore')
  })

  it('downloads, cancels, and removes grouped weights as one capability', async () => {
    const { backend, events } = makeBackend()
    expect(await settle(backend.deleteModel({ id: 'scriptGateLabels' }))).toBe('deleted')
    let view = await settle(backend.listModels())
    expect(view.models.filter((model) => ['scriptGate', 'scriptGateLabels'].includes(model.id)).every((model) => !model.installed)).toBe(true)

    expect(await begin(backend.downloadModel({ id: 'scriptGateLabels' }))).toBe('started')
    expect(events.some((event) => event.type === 'model-progress' && event.id === 'scriptGate')).toBe(true)
    expect(events.some((event) => event.type === 'model-progress' && event.id === 'scriptGateLabels')).toBe(true)
    expect(await settle(backend.cancelDownload({ id: 'scriptGateLabels' }))).toBe(true)
    expect(events.some((event) => event.type === 'model-progress' && event.id === 'scriptGate' && event.done && event.error === 'cancelled')).toBe(true)

    expect(await settle(backend.downloadModel({ id: 'scriptGate' }))).toBe('started')
    expect(events.some((event) => event.type === 'model-progress' && event.id === 'scriptGate' && event.done && event.total === null && !event.error)).toBe(true)
    view = await settle(backend.listModels())
    expect(view.models.filter((model) => ['scriptGate', 'scriptGateLabels'].includes(model.id)).every((model) => model.installed)).toBe(true)
  })

  it('reports a download on the event channel and ends with exactly one done', async () => {
    const { backend, events } = makeBackend()
    await settle(backend.downloadModel({ id: 'inpainter' }))

    const progress = events.filter((event) => event.type === 'model-progress')
    expect(progress.length).toBeGreaterThan(2)
    expect(progress[0]).toMatchObject({ id: 'inpainter', downloaded: 0, done: false })
    expect(progress.filter((event) => event.done)).toHaveLength(1)
    expect(progress.at(-1)).toMatchObject({ done: true, error: null })

    // The one row that was missing and gates something is now installed. The
    // readers' rows are still absent - they were not what was downloaded
    // - which is exactly the state a real machine sits in.
    const view = await settle(backend.listModels())
    expect(view.models.find((model) => model.id === 'inpainter')?.installed).toBe(true)
    const missing = view.models.filter((model) => !model.installed).map((model) => model.id)
    expect(missing).toEqual(['ocrEncoder', 'ocrDecoder', 'ocrVocab', 'hayaiVision', 'hayaiDecoder', 'hayaiTokenizer', 'pageDenoiseModel', 'pageDenoiseSeams'])
  })

  it('ends a cancelled download the same way a failed one ends', async () => {
    const { backend, events } = makeBackend()
    await begin(backend.downloadModel({ id: 'inpainter' }))
    await begin(backend.cancelDownload({ id: 'inpainter' }))

    const done = events.filter((event) => event.type === 'model-progress' && event.done)
    expect(done).toHaveLength(1)
    expect(done[0].error).toBe('cancelled')
    // And the row is where it started, not half-installed.
    const view = await settle(backend.listModels())
    expect(view.models.find((model) => model.id === 'inpainter').installed).toBe(false)
  })

  /**
   * What a stopped download leaves, said out loud. The bytes
   * are kept so the next press can resume from them, which makes them worth
   * reporting and worth being able to give back.
   */
  it('reports what a cancelled download left, and discards it on request', async () => {
    const { backend } = makeBackend()
    await begin(backend.downloadModel({ id: 'inpainter' }))
    await begin(backend.cancelDownload({ id: 'inpainter' }))

    const stopped = await settle(backend.listModels())
    const row = stopped.models.find((model) => model.id === 'inpainter')
    expect(row.installed).toBe(false)
    expect(row.partialBytes).toBeGreaterThan(0)
    expect(row.partialBytes).toBeLessThan(row.bytes)
    // Every other row has nothing beside it, which is the ordinary state.
    expect(stopped.models.filter((model) => model.partialBytes !== null)).toHaveLength(1)

    expect(await settle(backend.discardPartial({ id: 'inpainter' }))).toBe(true)
    const after = await settle(backend.listModels())
    expect(after.models.find((model) => model.id === 'inpainter').partialBytes).toBe(null)
    // And a second press has nothing to throw away, which is not a failure.
    expect(await settle(backend.discardPartial({ id: 'inpainter' }))).toBe(false)
  })

  it('resumes from the bytes it kept rather than starting again', async () => {
    const { backend, events } = makeBackend()
    await begin(backend.downloadModel({ id: 'inpainter' }))
    await begin(backend.cancelDownload({ id: 'inpainter' }))
    const kept = (await settle(backend.listModels())).models.find(
      (model) => model.id === 'inpainter',
    ).partialBytes

    events.length = 0
    await begin(backend.downloadModel({ id: 'inpainter' }))
    // The first event of a resumed transfer carries the prefix, so a bar
    // picking it back up starts where it stopped.
    expect(events[0]).toMatchObject({ type: 'model-progress', downloaded: kept, done: false })
    await begin(backend.cancelDownload({ id: 'inpainter' }))
  })

  it('will not discard the bytes a running download is writing into', async () => {
    const { backend } = makeBackend()
    await begin(backend.downloadModel({ id: 'inpainter' }))
    // Another window's press against a row drawn before this transfer started.
    // Deleting the `.part` under a writer is the one thing the press must not
    // do, so it does nothing and says so.
    expect(await begin(backend.discardPartial({ id: 'inpainter' }))).toBe(false)
    await begin(backend.cancelDownload({ id: 'inpainter' }))
    expect(await begin(backend.discardPartial({ id: 'inpainter' }))).toBe(true)
  })

  it('refuses to delete the runtime while it is downloading', async () => {
    const { backend } = makeBackend()
    await begin(backend.downloadRuntime())
    // The native command removes the whole runtimes tree, and a transfer's
    // `.part` is inside it.
    expect(await settle(backend.deleteRuntime())).toBe('busy')

    await begin(backend.cancelDownload({ id: 'runtime' }))
    expect(await settle(backend.deleteRuntime())).toBe('deleted')
  })

  it('takes an installed weight away again, and says which of the three things happened', async () => {
    const { backend } = makeBackend()
    expect(await settle(backend.deleteModel({ id: 'textDetector' }))).toBe('deleted')
    // Twice is not an error and it is not a silence either: the second press is
    // a stale row, and the answer says so.
    expect(await settle(backend.deleteModel({ id: 'textDetector' }))).toBe('notFound')
    const view = await settle(backend.listModels())
    expect(view.models.find((model) => model.id === 'textDetector').installed).toBe(false)
  })

  /**
   * The two refusals which are what a second window's stale
   * row gets when it presses Download: one already running, one already here.
   */
  it('says why a download did not start', async () => {
    const { backend } = makeBackend()
    // Every row but the redraw model is installed in the mock, so this is the
    // already-installed answer without arranging anything.
    expect(await settle(backend.downloadModel({ id: 'textDetector' }))).toBe('alreadyInstalled')

    expect(await begin(backend.downloadModel({ id: 'inpainter' }))).toBe('started')
    expect(await begin(backend.downloadModel({ id: 'inpainter' }))).toBe('alreadyRunning')
    await begin(backend.cancelDownload({ id: 'inpainter' }))
  })

  /**
   * The runtime is an archive rather than a weight and its row says what its
   * download costs before anything is fetched. The mock reports
   * one flavour because macOS publishes one, which is why the picker is not
   * drawn in a browser.
   */
  it('reports the runtime download size and the builds there are to choose from', async () => {
    const { backend } = makeBackend()
    const { runtime } = await settle(backend.listModels())

    expect(runtime.bytes).toBeGreaterThan(0)
    expect(runtime.flavours).toHaveLength(1)
    expect(runtime.flavours[0]).toMatchObject({ id: 'stock', isDefault: true, userInstalled: [] })
    expect(runtime.flavour).toBe(runtime.flavours[0].id)
    expect(runtime.bytes).toBe(runtime.flavours[0].bytes)

    // What is *installed* is a second question the row now answers.
    // On this machine the two agree, which is why Settings draws no
    // "installed X, chosen Y" line in a browser.
    expect(runtime.installedFlavour).toBe(runtime.flavour)
    expect(runtime.installedVersion).toBe(runtime.version)
  })

  /**
   * An uninstalled runtime has no build to report, and `null` means *unknown*
   * rather than a claim about one - the same thing the native side answers for
   * a library it did not unpack itself.
   */
  it('says nothing about the installed build when there is no runtime', async () => {
    const { backend } = makeBackend()
    expect(await settle(backend.deleteRuntime())).toBe('deleted')

    const { runtime } = await settle(backend.listModels())
    expect(runtime.installed).toBe(false)
    expect(runtime.installedFlavour).toBe(null)
    expect(runtime.installedVersion).toBe(null)
    // The chosen build is still named: it is what the Download button fetches.
    expect(runtime.flavour).toBe('stock')
  })

  /**
   * The seam's one argument to `listModels`. It asks the native
   * side to offer the token to a credential store that refused this process
   * once, which a browser has none of - so what is asserted here is that the
   * mock takes the argument and answers the same view, rather than rejecting a
   * call the dialog makes on every open.
   */
  it('accepts the credential-store retry a dialog open asks for', async () => {
    const { backend } = makeBackend()
    const plain = await settle(backend.listModels())
    const retried = await settle(backend.listModels({ retryStore: true }))

    expect(retried).toEqual(plain)
    expect(retried.tokenStore).toBe('fileNoStore')
    // No unreachable store, so no reason to give for one.
    expect(retried.tokenStoreReason).toBe(null)
  })

  it('stores a token without ever handing it back', async () => {
    const { backend } = makeBackend()
    const written = await settle(backend.writeSettings({ hfToken: 'hf_secret' }))
    const view = await settle(backend.listModels())
    expect(view.hasToken).toBe(true)
    expect(JSON.stringify(view)).not.toContain('hf_secret')

    // Not through the settings snapshot either, which is the other direction
    // the same value could have come back in.
    expect(written).not.toHaveProperty('hfToken')
    const read = await settle(backend.readSettings())
    expect(read).not.toHaveProperty('hfToken')
    expect(JSON.stringify(read)).not.toContain('hf_secret')
  })
})

describe('inference config and cloud secrets in mock backend', () => {
  it('reads default local target and empty profile maps in memory', async () => {
    const { backend } = makeBackend()
    const config = await settle(backend.readInferenceConfig())

    expect(config).toEqual({
      schemaVersion: 1,
      selectedTarget: { type: 'local' },
      beamProfiles: {},
      modalProfiles: {},
    })
  })

  it('updates public inference configuration and returns isolated clone without secrets', async () => {
    const { backend } = makeBackend()
    const newConfig = {
      schemaVersion: 1,
      selectedTarget: { type: 'beam', profile_id: 'beam-1' },
      beamProfiles: {
        'beam-1': {
          id: 'beam-1',
          name: 'Beam Worker',
          endpointUrl: 'https://api.beam.cloud/v1',
          canonicalOrigin: 'https://api.beam.cloud',
          canonicalOriginFingerprint: 'a1b2c3d4',
          createdAtMs: 1000,
          updatedAtMs: 2000,
        },
      },
      modalProfiles: {},
    }

    const written = await settle(backend.writeInferenceConfig({ config: newConfig }))
    expect(written).toEqual(newConfig)
    expect(written).not.toBe(newConfig) // isolated clone

    // Mutating written object does not affect stored state
    written.selectedTarget = { type: 'local' }
    const readAgain = await settle(backend.readInferenceConfig())
    expect(readAgain.selectedTarget).toEqual({ type: 'beam', profile_id: 'beam-1' })
    expect(JSON.stringify(readAgain)).not.toContain('secret')
  })

  it('rejects adversarial extra keys and secrets in writeInferenceConfig', async () => {
    const { backend } = makeBackend()

    // Adversarial top-level extra secret key
    await expect(
      settle(
        backend.writeInferenceConfig({
          config: {
            schemaVersion: 1,
            selectedTarget: { type: 'local' },
            beamProfiles: {},
            modalProfiles: {},
            secret: 'injected-top-level-secret',
          },
        }),
      ),
    ).rejects.toThrow(/unrecognized inference config field/)

    // Adversarial profile-level secret key
    await expect(
      settle(
        backend.writeInferenceConfig({
          config: {
            schemaVersion: 1,
            selectedTarget: { type: 'beam', profile_id: 'beam-1' },
            beamProfiles: {
              'beam-1': {
                id: 'beam-1',
                name: 'Beam Worker',
                endpointUrl: 'https://api.beam.cloud/v1',
                canonicalOrigin: 'https://api.beam.cloud',
                canonicalOriginFingerprint: 'a1b2c3d4',
                createdAtMs: 1000,
                updatedAtMs: 2000,
                secret: 'injected-profile-secret',
              },
            },
            modalProfiles: {},
          },
        }),
      ),
    ).rejects.toThrow(/unrecognized field 'secret' in beam profile/)

    // Ensure state remains clean
    const cleanRead = await settle(backend.readInferenceConfig())
    expect(JSON.stringify(cleanRead)).not.toContain('injected')
  })

  it('secret operations answer from a session-only store without a timer hang', async () => {
    const { backend } = makeBackend()
    const key = { provider: 'beam', profileId: 'beam-1', role: 'runtime' }

    expect(await settle(backend.storeCloudSecret({ ...key, secret: 'raw-secret' }))).toMatchObject({
      present: true,
      backend: 'session',
    })
    expect(await settle(backend.getCloudSecretSummary(key))).toMatchObject({ present: true })
    expect(await settle(backend.deleteCloudSecret(key))).toMatchObject({ present: false })
  })

  it('pins immutable model revision in getCloudModelInfo and default consent recipe', async () => {
    const { backend } = makeBackend()
    await settle(
      backend.writeInferenceConfig({
        config: {
          schemaVersion: 1,
          selectedTarget: { type: 'modal', profile_id: 'm1' },
          beamProfiles: {},
          modalProfiles: {
            m1: {
              id: 'm1',
              name: 'Modal Worker',
              endpointUrl: 'https://modal.run/v1',
              canonicalOrigin: 'https://modal.run',
              canonicalOriginFingerprint: 'fp-1',
              createdAtMs: 1,
              updatedAtMs: 1,
            },
          },
        },
      }),
    )

    const info = await settle(backend.getCloudModelInfo({ provider: 'modal', profileId: 'm1' }))
    expect(info.pinnedModelRevision).toBe('45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd')
    expect(info.pinnedModelRevision).not.toBe('main')

    await settle(backend.writeSettings({ cloudEngines: 'allowed' }))
    const proposal = await settle(
      backend.prepareCloudConsent({
        target: { type: 'modal', profile_id: 'm1' },
        intent: { action: 'applyTool', tool: 'contentAwareFill' },
      }),
    )
    expect(proposal.recipe.model_revision).toBe('45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd')
    expect(proposal.recipe.model_revision).not.toBe('main')
  })

  it('submitCloudAttempt requires valid grant, fails closed when missing/unknown, and prevents replay', async () => {
    const { backend } = makeBackend()
    await settle(
      backend.writeInferenceConfig({
        config: {
          schemaVersion: 1,
          selectedTarget: { type: 'modal', profile_id: 'm1' },
          beamProfiles: {},
          modalProfiles: {
            m1: {
              id: 'm1',
              name: 'Modal Worker',
              endpointUrl: 'https://modal.run/v1',
              canonicalOrigin: 'https://modal.run',
              canonicalOriginFingerprint: 'fp-1',
              createdAtMs: 1,
              updatedAtMs: 1,
            },
          },
        },
      }),
    )
    await settle(backend.writeSettings({ cloudEngines: 'allowed' }))

    // Missing grantNonce
    await expect(
      settle(backend.submitCloudAttempt({ attemptId: 'att-missing' })),
    ).rejects.toThrow(/invalid or missing grant/)

    // Empty grantNonce
    await expect(
      settle(backend.submitCloudAttempt({ attemptId: 'att-empty', grantNonce: '' })),
    ).rejects.toThrow(/invalid or missing grant/)

    // Unknown grantNonce
    await expect(
      settle(backend.submitCloudAttempt({ attemptId: 'att-unknown', grantNonce: 'grant-unknown' })),
    ).rejects.toThrow(/invalid or missing grant/)

    // Mint valid grant
    const proposal = await settle(
      backend.prepareCloudConsent({
        target: { type: 'modal', profile_id: 'm1' },
        intent: { action: 'applyTool', tool: 'contentAwareFill' },
      }),
    )
    const grant = await settle(
      backend.confirmCloudConsent({
        proposalId: proposal.proposalId,
        intent: { action: 'applyTool', tool: 'contentAwareFill' },
        rightsAttested: true,
        retentionAcknowledged: true,
      }),
    )

    // First attempt succeeds
    const first = await settle(
      backend.submitCloudAttempt({
        attemptId: 'att-valid-1',
        grantNonce: grant.nonce,
        snapshot: { regionRevision: 1, sourceImageHash: 'h1' },
      }),
    )
    expect(first.status).toBe('accepted')

    // Replay attempt fails closed
    await expect(
      settle(
        backend.submitCloudAttempt({
          attemptId: 'att-replay-1',
          grantNonce: grant.nonce,
          snapshot: { regionRevision: 1, sourceImageHash: 'h1' },
        }),
      ),
    ).rejects.toThrow(/replay detected/)
  })
})

describe('analysis cancellation', () => {
  it('rejects a pending analysis with the typed cancellation and resolves cancel', async () => {
    const { backend } = makeBackend()
    const pending = backend.analyzeCapabilities({ requestId: 'cancel-me' })
    const rejected = expect(pending).rejects.toBe('analysis cancelled')
    expect(await backend.cancelCapabilityAnalysis('cancel-me')).toBe(true)
    await rejected
    // Nothing by that id is running any more.
    expect(await backend.cancelCapabilityAnalysis('cancel-me')).toBe(false)
  })
})

/**
 * The text-shaped review's local half: analysis of the page `mockreview.js`
 * draws, and the one-component write, refused in the native order.
 */
describe('text-shaped review mock', () => {
  const ANALYSIS_MS = 1400
  const spec = (overrides = {}) => ({ chapterId: CHAPTER, pageIndex: 0, workflow: 'text_shape', rtProfile: 'full-halves',
    rtBackend: 'ort-cpu', samBackend: 'ort-webgpu', ...overrides })
  const reviewBackend = () => createMockBackend({ timing: { ...TIMING, analysis: ANALYSIS_MS } })
  let requests = 0
  const request = () => `mock-test-${++requests}`

  it('answers after its delay with the drawn page, its mask and its components', async () => {
    const backend = reviewBackend()
    const caps = await settle(backend.listWorkflowCapabilities())
    expect(caps).toMatchObject({ runtimeInstalled: true, fullRtInstalled: true, samInstalled: true, samWriteQualified: true })
    let landed = null
    const pending = backend.analyzeChapterPage(spec({ requestId: request() })).then((value) => { landed = value })
    await vi.advanceTimersByTimeAsync(ANALYSIS_MS - 1)
    expect(landed).toBeNull()
    await vi.advanceTimersByTimeAsync(1)
    await pending
    const { evidence } = landed
    expect(landed.samWriteEligible).toBe(true)
    expect(landed.analysisId).toMatch(/^[0-9a-f]{64}$/)
    expect(pngSize(landed.sourceDataUrl)).toEqual({ width: evidence.width, height: evidence.height })
    expect(pngSize(landed.maskDataUrl)).toEqual({ width: evidence.width, height: evidence.height })
    expect(evidence.components.length).toBeGreaterThan(3)
    // Reasons are explicit and sparse: most components are plain candidates.
    expect(evidence.components.some((component) => component.reviewReasons.includes('unassignedComponent'))).toBe(true)
    expect(evidence.components.some((component) => component.reviewReasons.length === 0)).toBe(true)
    expect(evidence.components.every((component) => !('reviewRequired' in component))).toBe(true)
    // One group per bubble, each component in at most one group.
    const members = evidence.groups.flatMap((group) => group.componentIds)
    expect(new Set(members).size).toBe(members.length)
    expect(evidence.groups.filter((group) => group.disposition === 'candidate').map((group) => group.reasons)).toEqual([['unassignedComponent']])
    for (const component of evidence.components) {
      if (component.groupId) expect(evidence.groups.find((group) => group.id === component.groupId).componentIds).toContain(component.id)
    }
    expect(evidence.components.some((component) => !component.rtBubbleIds.length)).toBe(true)
    expect(evidence.regions.some((region) => region.kind === 'bubble_context')).toBe(true)
    expect(evidence.regions.some((region) => region.detectorOnly)).toBe(true)
  })

  it('refuses a reused request id and a longstrip chapter as the native side does', async () => {
    const backend = reviewBackend()
    const id = request()
    const first = backend.analyzeChapterPage(spec({ requestId: id }))
    await expect(backend.analyzeChapterPage(spec({ requestId: id }))).rejects.toThrow('Analysis request id was already used')
    await settle(first)
    await expect(settle(backend.analyzeChapterPage(spec({ chapterId: 'neon-alley-ch4', requestId: request() }))))
      .rejects.toThrow('Chapter model analysis currently requires a paginated chapter')
  })

  it('keeps CPU and regions-only analyses from writing', async () => {
    const backend = reviewBackend()
    const cpu = await settle(backend.analyzeChapterPage(spec({ samBackend: 'ort-cpu', requestId: request() })))
    expect(cpu.samWriteEligible).toBe(false)
    await expect(settle(backend.prepareComponentWrite({ analysisId: cpu.analysisId, chapterId: CHAPTER, pageIndex: 0,
      componentId: 'sam-00001', allowOutsideBubbles: true }))).rejects.toThrow('not qualified for component writing')
    const regions = await settle(backend.analyzeChapterPage(spec({ workflow: 'regions', requestId: request() })))
    expect(regions).toMatchObject({ analysisId: null, samBackend: null, maskDataUrl: null })
    expect(regions.evidence.components).toEqual([])
    expect(regions.evidence.regions.every((region) => region.detectorOnly)).toBe(true)
  })

  it('prepares, applies and reloads one component, and holds one outside every bubble', async () => {
    const backend = reviewBackend()
    const analysis = await settle(backend.analyzeChapterPage(spec({ requestId: request() })))
    const inside = analysis.evidence.components.find((component) => component.rtBubbleIds.length)
    const outside = analysis.evidence.components.find((component) => !component.rtBubbleIds.length)
    const write = (componentId, extra = {}) => ({ analysisId: analysis.analysisId, chapterId: CHAPTER, pageIndex: 0,
      componentId, allowOutsideBubbles: false, paddingPx: 0, correctionRevision: 0, ...extra })

    await expect(settle(backend.prepareComponentWrite(write(outside.id))))
      .rejects.toThrow('Outside-bubble component is held until explicitly permitted')
    await expect(settle(backend.prepareComponentWrite(write('rt-0000', { allowOutsideBubbles: true }))))
      .rejects.toThrow('Only a SAM component can grant write support')

    const plan = await settle(backend.prepareComponentWrite(write(inside.id)))
    expect(plan.supportPixels).toBe(inside.pixels)
    expect(pngSize(plan.supportDataUrl)).toEqual({ width: plan.bounds.w, height: plan.bounds.h })
    const padded = await settle(backend.prepareComponentWrite(write(inside.id, { paddingPx: 2 })))
    expect(padded.supportPixels).toBeGreaterThan(plan.supportPixels)

    await expect(settle(backend.applyComponentWrite({ planId: plan.planId, approvedSupportSha256: plan.supportSha256 })))
      .rejects.toThrow('Approval does not match the prepared support raster')
    const applied = await settle(backend.applyComponentWrite({ planId: padded.planId, approvedSupportSha256: padded.supportSha256 }))
    expect(applied.regionId).toMatch(new RegExp(`-hreview-${inside.id}$`))
    const saved = await settle(backend.loadComponentCorrection({ analysisId: analysis.analysisId, componentId: inside.id }))
    expect(saved).toMatchObject({ regionId: applied.regionId, paddingPx: 2, correctionRevision: 0, planRevision: 1 })

    // A different correction needs a new revision.
    const additions = { bounds: { x: inside.bounds.x, y: inside.bounds.y, w: 1, h: 1 }, bits: [255] }
    await expect(settle(backend.prepareComponentWrite(write(inside.id, { additions }))))
      .rejects.toThrow('Mask corrections changed without a new correction revision')
    const next = await settle(backend.prepareComponentWrite(write(inside.id, { additions, correctionRevision: 1 })))
    expect(next.correctionRevision).toBe(1)
  })
})

/**
 * The runtime's load, as `diagnostics` reports it. The native side loads the
 * library to answer; the mock has nothing to load, so it answers from the
 * catalogue and a `?runtimeLoad=` knob stands in for a machine that refuses.
 */
describe('diagnostics in mock backend', () => {
  afterEach(() => vi.unstubAllGlobals())

  /** @param {any} answer */
  const runtimeOf = (answer) => answer.components.find((/** @type {any} */ c) => c.name === 'onnxruntime')

  it('loads an installed runtime and reports a deleted one as missing', async () => {
    const { backend } = makeBackend()
    expect(runtimeOf(await settle(backend.diagnostics()))).toEqual({
      name: 'onnxruntime', available: true, detail: null, reasonKey: null,
    })
    expect(await settle(backend.deleteRuntime())).toBe('deleted')
    expect(runtimeOf(await settle(backend.diagnostics()))).toMatchObject({
      available: false, reasonKey: 'diagnostics.runtime.missing',
    })
  })

  it('reports the load failure the knob names, and only a known one', async () => {
    vi.stubGlobal('location', { search: '?runtimeLoad=quarantined' })
    const { backend } = makeBackend()
    expect(runtimeOf(await settle(backend.diagnostics()))).toMatchObject({
      available: false, reasonKey: 'diagnostics.runtime.quarantined',
    })
    vi.stubGlobal('location', { search: '?runtimeLoad=toString' })
    expect(runtimeOf(await settle(backend.diagnostics()))).toMatchObject({ available: true, reasonKey: null })
  })
})

describe('replacing pages with denoised pages, in the mock', () => {
  const chapterOf = async (backend) =>
    (await settle(backend.listProjects())).flatMap((project) => project.chapters).find((entry) => entry.id === CHAPTER)

  it('is offered once a page has a denoised file, leaves the rest raw, and keeps the regions', async () => {
    const backend = createMockBackend({ timing: { method: 0, analysis: 0, region: 0, pageTail: 0 } })
    await settle(backend.downloadModelGroup({ id: 'pageDenoise' }))
    await vi.runAllTimersAsync()
    const models = await settle(backend.listModels())
    expect(models.models.filter((row) => row.requiredBy.includes('pageDenoise')).every((row) => row.installed)).toBe(true)
    const before = await chapterOf(backend)
    expect(before.denoiseReplacement).toBeNull()

    await settle(backend.denoiseChapterLocal({ runId: 'den-1', chapterId: CHAPTER, pageIndices: [0, 1],
      presetId: 'waifu2x-scan-4x-n2', outDir: '/scans/denoised' }))
    expect((await chapterOf(backend)).denoiseReplacement, 'the pages never denoised stay raw')
      .toEqual({ pages: 2, kept: 0, missing: before.pages.length - 2 })

    await settle(backend.denoiseChapterLocal({ runId: 'den-2', chapterId: CHAPTER, presetId: 'waifu2x-scan-4x-n2',
      outDir: '/scans/denoised' }))
    const denoised = await chapterOf(backend)
    expect(denoised.denoiseReplacement).toEqual({ pages: before.pages.length, kept: 0, missing: 0 })

    const report = await settle(backend.replaceWithDenoised({ chapterId: CHAPTER }))
    expect(report).toEqual({ replaced: before.pages.map((page) => page.index), kept: [], missing: [], failed: [] })
    const after = await chapterOf(backend)
    expect(after.denoiseReplacement).toBeNull()
    after.pages.forEach((page, at) => {
      expect(page.sourceSha).not.toBe(before.pages[at].sourceSha)
      expect(page.regionCount).toBe(before.pages[at].regionCount)
    })
    const again = await settle(backend.replaceWithDenoised({ chapterId: CHAPTER }))
    expect(again.replaced, 'a taken file is not taken twice').toEqual([])
    expect(again.missing).toEqual(before.pages.map((page) => page.index))
  })

  it('offers the pages a failed run did write', async () => {
    vi.stubGlobal('location', { search: '?denoise=pageFail' })
    const backend = createMockBackend({ timing: { method: 0, analysis: 0, region: 0, pageTail: 0 } })
    await settle(backend.downloadModelGroup({ id: 'pageDenoise' }))
    await vi.runAllTimersAsync()
    const report = await settle(backend.denoiseChapterLocal({ runId: 'den-f', chapterId: CHAPTER,
      presetId: 'waifu2x-scan-4x-n2', outDir: '/scans/denoised' }))
    expect(report.failed.map((failure) => failure.pageIndex)).toEqual([2])
    const chapter = await chapterOf(backend)
    expect(chapter.denoiseReplacement).toEqual({ pages: chapter.pages.length - 1, kept: 0, missing: 1 })
    const replaced = await settle(backend.replaceWithDenoised({ chapterId: CHAPTER }))
    expect(replaced.missing).toEqual([2])
    vi.unstubAllGlobals()
  })
})

describe('the denoise history of a chapter, in the mock', () => {
  const chapterOf = async (backend) =>
    (await settle(backend.listProjects())).flatMap((project) => project.chapters).find((entry) => entry.id === CHAPTER)

  it('keeps each run, names the newest, and keeps a taken run as history', async () => {
    const backend = createMockBackend({ timing: { method: 0, analysis: 0, region: 0, pageTail: 0 } })
    await settle(backend.downloadModelGroup({ id: 'pageDenoise' }))
    await vi.runAllTimersAsync()
    expect((await chapterOf(backend)).denoiseHistory).toBeNull()

    await settle(backend.denoiseChapterLocal({ runId: 'den-1', chapterId: CHAPTER, pageIndices: [0, 1],
      presetId: 'waifu2x-scan-4x-n2', outDir: '/scans/denoised' }))
    await settle(backend.denoiseChapterLocal({ runId: 'den-2', chapterId: CHAPTER, presetId: 'waifu2x-scan-4x-n2',
      outDir: '/scans/denoised-2' }))
    const summary = (await chapterOf(backend)).denoiseHistory
    expect(summary).toMatchObject({ runs: 2, taken: false })

    const runs = await settle(backend.denoiseHistory({ chapterId: CHAPTER }))
    expect(runs.map((run) => run.created)).toEqual([summary.latest, summary.latest - 1])
    expect(runs[0]).toMatchObject({ preset: 'waifu2x-scan-4x-n2', target: 'local', fromCleaned: false, folder: '/scans/denoised-2' })
    expect(runs[1].pages.map((page) => [page.pageIndex, page.current])).toEqual([[0, false], [1, false]])
    expect(runs[0].pages.every((page) => page.exists && page.current && !page.taken)).toBe(true)

    await settle(backend.replaceWithDenoised({ chapterId: CHAPTER }))
    const after = await settle(backend.denoiseHistory({ chapterId: CHAPTER }))
    expect(after.map((run) => run.created)).toEqual(runs.map((run) => run.created))
    expect(after[0].pages.every((page) => page.taken && !page.current)).toBe(true)
    expect((await chapterOf(backend)).denoiseHistory).toMatchObject({ runs: 2, taken: true })
  })

  it('serves a compare image only for a page and run it holds', async () => {
    const backend = createMockBackend({ timing: { method: 0, analysis: 0, region: 0, pageTail: 0 } })
    await settle(backend.downloadModelGroup({ id: 'pageDenoise' }))
    await vi.runAllTimersAsync()
    await settle(backend.denoiseChapterLocal({ runId: 'den-1', chapterId: CHAPTER, pageIndices: [0],
      presetId: 'waifu2x-scan-4x-n2', outDir: '/scans/denoised' }))
    const [run] = await settle(backend.denoiseHistory({ chapterId: CHAPTER }))
    const raw = new TextDecoder().decode(await settle(backend.denoiseCompareImage({ chapterId: CHAPTER, pageIndex: 0, run: run.created, side: 'raw' })))
    const clean = new TextDecoder().decode(await settle(backend.denoiseCompareImage({ chapterId: CHAPTER, pageIndex: 0, run: run.created, side: 'denoised' })))
    expect(raw.startsWith('<svg')).toBe(true)
    expect(raw).toContain('feTurbulence')
    expect(clean).not.toContain('feTurbulence')
    await expect(settle(backend.denoiseCompareImage({ chapterId: CHAPTER, pageIndex: 1, run: run.created, side: 'raw' })))
      .rejects.toThrow('denoise_run_missing')
    await expect(settle(backend.denoiseCompareImage({ chapterId: CHAPTER, pageIndex: 0, run: run.created + 5, side: 'raw' })))
      .rejects.toThrow('denoise_run_missing')
    await expect(settle(backend.denoiseCompareImage({ chapterId: CHAPTER, pageIndex: 0, run: run.created, side: '/etc/passwd' })))
      .rejects.toThrow('denoise_side_invalid')
  })
})
