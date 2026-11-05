/**
 * The browser mock's Nano Banana seam, held to what the native side decides:
 * the key is the instance's own, a render needs cloud engines on, a live grant
 * with a region left and a stored key, a grant is withdrawn when cloud engines
 * go off and lapses when idle, and a region with a layer is not sent again.
 * Its own finishing is read from settings, recorded on the layer, kept by Try
 * again, and changed afterwards from the kept answer.
 */
import { describe, expect, it, vi } from 'vitest'

import { createMockBackend } from './mock.js'
import { DEFAULT_NANO_BANANA_MODEL } from '../model/nanobanana.js'

const CHAPTER = 'tsuki-to-hane-ch107'
const KEY = 'AIzaSyTestKeyOnly0123456789'
const ZERO = { method: 0, cloud: 0, provision: 0, analysis: 0, region: 0, pageTail: 0, noticeStagger: 0 }

/** @param {Record<string, any>} [options] */
function makeMock(options = {}) {
  return /** @type {any} */ (createMockBackend({ timing: ZERO, ...options }))
}

/** @param {any} mock */
async function detected(mock) {
  let done = () => {}
  const finished = new Promise((resolve) => { done = resolve })
  const stop = mock.subscribe((/** @type {any} */ event) => { if (event.type === 'run-finished') done() })
  const handle = await mock.runClean({ scope: 'page', chapterId: CHAPTER, pageIndex: 0, mode: 'detect' })
  if (handle?.runId) await finished
  stop()
  const [page] = await mock.loadPages({ chapterId: CHAPTER, indices: [0] })
  return page.regions.filter((/** @type {any} */ region) => region.outcome === 'detected')
}

/** @param {any} mock */
function refusals(mock) {
  /** @type {string[]} */
  const reasons = []
  mock.subscribe((/** @type {any} */ event) => {
    if (event.key === 'notice.mask.rerunFailed') reasons.push(event.params.reasonKey)
  })
  return reasons
}

/** @param {any} mock @param {any} region @param {string} [grant] */
const clean = (mock, region, grant) => mock.applyTool({
  tool: 'autoClean', chapterId: CHAPTER, pageIndex: 0, regionId: region.id,
  params: { engine: 'nanobanana', ...(grant ? { nanoBananaGrant: grant } : {}) },
})

/** @param {Record<string, any>} [options] */
async function ready(options) {
  const mock = makeMock(options)
  await mock.writeSettings({ cloudEngines: 'allowed' })
  await mock.storeNanoBananaKey({ key: KEY })
  return mock
}

describe('the Nano Banana key on the mock', () => {
  it('is the instance\'s own', async () => {
    const one = makeMock()
    const other = makeMock()
    expect(await one.storeNanoBananaKey({ key: KEY })).toEqual({ sessionOnly: false })
    expect(await one.nanoBananaHasKey()).toBe(true)
    expect(await other.nanoBananaHasKey()).toBe(false)
  })

  it('answers where it was kept, and refuses as natively', async () => {
    const session = makeMock({ nanoBananaKeyStore: 'session' })
    await expect(session.nanoBananaHasKey()).rejects.toThrow('nanoBanana.key.noStore')
    expect(await session.storeNanoBananaKey({ key: KEY })).toEqual({ sessionOnly: true })
    expect(await session.nanoBananaHasKey()).toBe(true)
    for (const key of ['  ', 'short', `${KEY} with spaces`, 'x'.repeat(201)]) {
      await expect(makeMock().storeNanoBananaKey({ key })).rejects.toThrow('nanoBanana.key.invalid')
    }
    expect(await makeMock().storeNanoBananaKey({ key: `  ${KEY}  ` })).toEqual({ sessionOnly: false })
    await expect(makeMock({ nanoBananaKeyStore: 'failed' }).storeNanoBananaKey({ key: KEY })).rejects.toThrow('nanoBanana.key.failed')
  })
})

describe('a Nano Banana grant on the mock', () => {
  it('is refused while cloud engines are off, for another model, and for no regions', async () => {
    const mock = makeMock()
    await expect(mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })).rejects.toThrow('decline.reason.nanoBananaOff')
    await mock.writeSettings({ cloudEngines: 'allowed' })
    await expect(mock.grantNanoBanana({ model: 'gemini-3-pro-image', count: 1 })).rejects.toThrow('decline.reason.nanoBananaNoConsent')
    await expect(mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 0 })).rejects.toThrow('decline.reason.nanoBananaNoConsent')
  })

  it('renders nothing without it, and as many regions as it covers with it', async () => {
    const mock = await ready()
    const reasons = refusals(mock)
    const [first, second] = await detected(mock)
    expect(await clean(mock, first)).toEqual({ status: 'blocked' })
    const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    const done = await clean(mock, first, grant)
    expect(done.status).toBe('applied')
    expect(done.mask.provenance.engine).toBe('nanobanana')
    expect(done.mask.provenance.params_snapshot.nano_banana).toEqual({
      model: DEFAULT_NANO_BANANA_MODEL, answer_sha256: expect.any(String),
    })
    // The one region is spent.
    expect(await clean(mock, second, grant)).toEqual({ status: 'blocked' })
    await vi.waitFor(() => expect(reasons).toEqual(['decline.reason.nanoBananaNoConsent', 'decline.reason.nanoBananaNoConsent']))
  })

  it('sends the grant it holds when the caller names none, as the Tauri adapter does', async () => {
    const mock = await ready()
    const [first, second] = await detected(mock)
    await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    expect((await clean(mock, first)).status).toBe('applied')
    expect(await clean(mock, second)).toEqual({ status: 'blocked' })
    // A grant named that is not the held one is still refused.
    await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    expect(await clean(mock, second, 'mock-nano-banana-grant-0')).toEqual({ status: 'blocked' })
  })

  it('records the model the window confirmed, not one picked after it', async () => {
    const mock = await ready()
    const [first] = await detected(mock)
    const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    await mock.writeSettings({ nanoBananaModel: 'gemini-3-pro-image' })
    const done = await clean(mock, first, grant)
    expect(done.mask.provenance.params_snapshot.nano_banana.model).toBe(DEFAULT_NANO_BANANA_MODEL)
  })

  it('is withdrawn when cloud engines go off, and says so when no key is stored', async () => {
    const mock = makeMock()
    const reasons = refusals(mock)
    await mock.writeSettings({ cloudEngines: 'allowed' })
    const [first] = await detected(mock)
    let { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 2 })
    expect(await clean(mock, first, grant)).toEqual({ status: 'blocked' })
    await mock.writeSettings({ cloudEngines: 'blocked' })
    expect(await clean(mock, first, grant)).toEqual({ status: 'blocked' })
    await mock.writeSettings({ cloudEngines: 'allowed' })
    expect(await clean(mock, first, grant)).toEqual({ status: 'blocked' })
    ;({ grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 }))
    await mock.nanoBananaStop()
    expect(await clean(mock, first, grant)).toEqual({ status: 'blocked' })
    await vi.waitFor(() => expect(reasons).toEqual([
      'decline.reason.nanoBananaNoKey',
      'decline.reason.nanoBananaOff',
      'decline.reason.nanoBananaNoConsent',
      'decline.reason.nanoBananaNoConsent',
    ]))
  })

  it('lapses when idle', async () => {
    let now = 1_000_000
    const timers = { setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout, now: () => now }
    const mock = await ready({ timers })
    const [first] = await detected(mock)
    const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 5 })
    now += 10 * 60 * 1000
    expect(await clean(mock, first, grant)).toEqual({ status: 'blocked' })
  })

  it('sends nothing over a region that has a layer now', async () => {
    const mock = await ready()
    const [first, second] = await detected(mock)
    const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    // Cleaned by hand while the batch ran.
    await mock.applyTool({ tool: 'autoClean', chapterId: CHAPTER, pageIndex: 0, regionId: first.id, params: { engine: 'fill' } })
    expect(await clean(mock, first, grant)).toEqual({ status: 'not-found' })
    const cleanedAnyway = await mock.cleanAnyway({ regionId: first.id, engine: 'nanobanana', params: { nanoBananaGrant: grant } })
    expect(cleanedAnyway).toBeNull()
    // The region was not taken from the grant.
    expect((await clean(mock, second, grant)).status).toBe('applied')
  })

  it('holds Try again on a Nano Banana layer to a grant, and steps off it for free', async () => {
    const mock = await ready()
    const [first] = await detected(mock)
    let { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    const done = await clean(mock, first, grant)
    const maskId = done.mask.id
    expect(await mock.rerunMask({ maskId, kind: 'retry' })).toBeNull()
    ;({ grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 }))
    const again = await mock.rerunMask({ maskId, kind: 'retry', params: { nanoBananaGrant: grant } })
    expect(again?.mask.provenance.engine).toBe('nanobanana')
    const simpler = await mock.rerunMask({ maskId: again.mask.id, kind: 'simpler' })
    expect(simpler?.mask.provenance.engine).toBe('flux')
  })
})

/**
 * Start a batch over `regions` and wait for its end. Answers the events in
 * order (type, or the notice key), the report and the run's summary.
 *
 * @param {any} mock @param {any[]} regions @param {{count?: number, onStart?: (runId: string) => void}} [options]
 */
async function batch(mock, regions, { count = regions.length, onStart } = {}) {
  const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count })
  /** @type {string[]} */
  const seen = []
  /** @type {any} */
  let report = null
  let done = (/** @type {any} */ _summary) => {}
  const finished = new Promise((resolve) => { done = resolve })
  const stop = mock.subscribe((/** @type {any} */ event) => {
    if (event.type === 'history-changed') return
    seen.push(event.type === 'notice' ? event.key : event.type)
    if (event.type === 'nano-banana-report') report = event.report
    if (event.type === 'run-finished') done(event)
  })
  const handle = await mock.startNanoBananaRun({ chapterId: CHAPTER, regionIds: regions.map((region) => region.id), nanoBananaGrant: grant })
  onStart?.(handle.runId)
  const summary = await finished
  stop()
  return { seen, report, summary, handle, grant }
}

describe('a Nano Banana batch on the mock', () => {
  it('cleans every region, says so, and is one undo step', async () => {
    const mock = await ready()
    const regions = await detected(mock)
    const before = (await mock.historyLoad({ chapterId: CHAPTER })).entries.length
    const { seen, report, summary } = await batch(mock, regions)
    expect(summary.reason).toBe('completed')
    expect(report).toMatchObject({ total: regions.length, cleaned: regions.length, gone: 0, failed: [], left: [], byUser: false })
    expect(report.stopped).toBeUndefined()
    // The report and the notice come before the run's end, as natively.
    expect(seen.slice(-3)).toEqual(['nano-banana-report', 'nanoBanana.job.finished', 'run-finished'])
    expect(seen.filter((type) => type === 'region-done')).toHaveLength(regions.length)
    const history = await mock.historyLoad({ chapterId: CHAPTER })
    expect(history.entries.length).toBe(before + 1)
    const step = await mock.historyMove({ chapterId: CHAPTER, direction: 'undo' })
    expect(step.entry).toMatchObject({ op: 'clean-state', label: 'nanoBanana.job.undo' })
    expect(step.entry.parts.map((/** @type {any} */ part) => part.regionId)).toEqual(regions.map((region) => region.id))
  })

  it('skips a refused crop with its reason and goes on', async () => {
    const mock = await ready({ nanoBananaScenario: 'blocked' })
    const regions = await detected(mock)
    const { seen, report, summary } = await batch(mock, regions)
    expect(summary.reason).toBe('completed')
    expect(report.cleaned).toBe(regions.length - 1)
    expect(report.failed).toEqual([{ regionId: regions[0].id, pageIndex: 0, reason: 'decline.reason.nanoBananaBlocked' }])
    expect(seen).toContain('nanoBanana.job.finishedWithFailures')
  })

  it('stops at once on an account failure, and leaves the rest', async () => {
    const mock = await ready({ nanoBananaScenario: 'billing' })
    const regions = await detected(mock)
    expect(regions.length).toBeGreaterThan(2)
    const { seen, report, summary } = await batch(mock, regions)
    expect(summary.reason).toBe('failed')
    expect(report).toMatchObject({ cleaned: 1, stopped: 'decline.reason.nanoBananaBilling', byUser: false })
    expect(report.left.map((/** @type {any} */ target) => target.regionId)).toEqual(regions.slice(1).map((region) => region.id))
    expect(seen).toContain('nanoBanana.job.halted')
  })

  it('stops on Stop, withdraws the grant, and says the person stopped it', async () => {
    const mock = await ready({ timing: { ...ZERO, region: 20 } })
    const regions = await detected(mock)
    const { report, summary, grant } = await batch(mock, regions, { onStart: (runId) => { void mock.cancelRun({ runId }) } })
    expect(summary.reason).toBe('cancelled')
    expect(report).toMatchObject({ stopped: 'decline.reason.nanoBananaStopped', byUser: true })
    expect(report.cleaned + report.left.length).toBe(regions.length)
    // The grant went with the Stop: nothing more goes on it.
    const reasons = refusals(mock)
    await clean(mock, regions[regions.length - 1], grant)
    expect(reasons).toEqual(['decline.reason.nanoBananaNoConsent'])
  })

  it('refuses before reserving anything when the grant covers fewer regions', async () => {
    const mock = await ready()
    const regions = await detected(mock)
    const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    await expect(mock.startNanoBananaRun({ chapterId: CHAPTER, regionIds: regions.map((region) => region.id), nanoBananaGrant: grant }))
      .rejects.toThrow('decline.reason.nanoBananaNoConsent')
    await expect(mock.startNanoBananaRun({ chapterId: CHAPTER, regionIds: [], nanoBananaGrant: grant })).rejects.toThrow('nano_banana_no_regions')
    expect(await mock.listJobs()).toEqual([])
  })

  it('refuses an undo of its step while it still runs', async () => {
    const mock = await ready({ timing: { ...ZERO, region: 30 } })
    const regions = await detected(mock)
    let refused = /** @type {any} */ (null)
    let firstDone = () => {}
    const cleanedOne = new Promise((resolve) => { firstDone = () => resolve(undefined) })
    mock.subscribe((/** @type {any} */ event) => { if (event.type === 'region-done') firstDone() })
    const { summary } = await batch(mock, regions, {
      onStart: () => {
        void cleanedOne.then(() => mock.historyMove({ chapterId: CHAPTER, direction: 'undo' }).catch((/** @type {any} */ error) => { refused = error }))
      },
    })
    expect(summary.reason).toBe('completed')
    expect(String(refused?.message)).toMatch(/^run_step_in_progress/)
  })
})

describe('Nano Banana finishing on the mock', () => {
  /** A Nano Banana layer on the first detection, made with `settings`. @param {Record<string, any>} [settings] */
  async function layer(settings = {}) {
    const mock = await ready()
    await mock.writeSettings(settings)
    const [first] = await detected(mock)
    const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    const done = await clean(mock, first, grant)
    return { mock, done, regionId: first.id }
  }

  it('refuses a finishing native refuses, and writes nothing', async () => {
    const mock = makeMock()
    for (const patch of [{ nanoBananaFinishing: 'blur' }, { nanoBananaFinishingGrayscale: 'yes' }, { nanoBananaStrays: 1 }]) {
      await expect(mock.writeSettings(patch)).rejects.toThrow()
    }
    expect(await mock.readSettings()).not.toHaveProperty('nanoBananaFinishing')
  })

  it('finishes by its own setting, not the shared one, and records it', async () => {
    const { done } = await layer({
      cloudFinishing: 'poisson', finishingEveryModel: true, nanoBananaFinishing: 'tone-only', nanoBananaStrays: true,
    })
    expect(done.mask.provenance.params_snapshot.postprocess).toEqual({ finishing: 'tone-only', grayscale: false, strays: true })
    const plain = await layer()
    expect(plain.done.mask.provenance.params_snapshot.postprocess).toEqual({ finishing: 'copy', grayscale: false })
  })

  it('keeps the layer\'s finishing on Try again, whatever the setting says by then', async () => {
    const { mock, done } = await layer({ nanoBananaFinishing: 'dots-only', nanoBananaFinishingGrayscale: true })
    await mock.writeSettings({ nanoBananaFinishing: 'as-is', nanoBananaFinishingGrayscale: false })
    const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    const again = await mock.rerunMask({ maskId: done.mask.id, kind: 'retry', params: { nanoBananaGrant: grant } })
    expect(again.mask.provenance.params_snapshot.postprocess).toEqual({ finishing: 'dots-only', grayscale: true })
  })

  it('takes the setting on a re-clean picked by name, which is not Try again', async () => {
    const { mock, done } = await layer({ nanoBananaFinishing: 'dots-only' })
    await mock.writeSettings({ nanoBananaFinishing: 'tone-only' })
    const { grant } = await mock.grantNanoBanana({ model: DEFAULT_NANO_BANANA_MODEL, count: 1 })
    const again = await mock.rerunMask({ maskId: done.mask.id, kind: 'engine', engine: 'nanobanana', params: { nanoBananaGrant: grant } })
    expect(again.mask.provenance.params_snapshot.postprocess).toEqual({ finishing: 'tone-only', grayscale: false })
  })

  it('is changed afterwards from the kept answer, its stray choice kept', async () => {
    const { mock, regionId } = await layer({ nanoBananaStrays: true })
    const previews = await mock.regionFinishingPreviews({ regionId })
    expect(previews.unavailable).toBeNull()
    expect(previews.strays).toBe(true)
    expect(previews.current).toEqual({ finishing: 'copy', grayscale: false, strays: true })
    const change = await mock.setRegionFinishing({ regionId, finishing: 'poisson', grayscale: false, strays: previews.strays })
    expect(change.history).not.toBeNull()
    expect(change.region.mask.provenance.params_snapshot.postprocess).toEqual({ finishing: 'poisson', grayscale: false, strays: true })
  })
})
