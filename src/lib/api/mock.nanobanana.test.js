/**
 * The browser mock's Nano Banana seam, held to what the native side decides:
 * the key is the instance's own, a render needs cloud engines on, a live grant
 * with a region left and a stored key, a grant is withdrawn when cloud engines
 * go off and lapses when idle, and a region with a layer is not sent again.
 */
import { describe, expect, it, vi } from 'vitest'

import { createMockBackend } from './mock.js'
import { DEFAULT_NANO_BANANA_MODEL } from '../model/nanobanana.js'

const CHAPTER = 'tsuki-to-hane-ch107'
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
  await mock.storeNanoBananaKey({ key: 'AIza-test' })
  return mock
}

describe('the Nano Banana key on the mock', () => {
  it('is the instance\'s own', async () => {
    const one = makeMock()
    const other = makeMock()
    expect(await one.storeNanoBananaKey({ key: 'AIza-test' })).toEqual({ sessionOnly: false })
    expect(await one.nanoBananaHasKey()).toBe(true)
    expect(await other.nanoBananaHasKey()).toBe(false)
  })

  it('answers where it was kept, and refuses as natively', async () => {
    expect(await makeMock({ nanoBananaKeyStore: 'session' }).storeNanoBananaKey({ key: 'k' })).toEqual({ sessionOnly: true })
    await expect(makeMock().storeNanoBananaKey({ key: '  ' })).rejects.toThrow('nanoBanana.key.invalid')
    await expect(makeMock({ nanoBananaKeyStore: 'none' }).storeNanoBananaKey({ key: 'k' })).rejects.toThrow('nanoBanana.key.noStore')
    await expect(makeMock({ nanoBananaKeyStore: 'failed' }).storeNanoBananaKey({ key: 'k' })).rejects.toThrow('nanoBanana.key.failed')
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
    expect(done.mask.provenance.params_snapshot.nano_banana).toEqual({ model: DEFAULT_NANO_BANANA_MODEL })
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
