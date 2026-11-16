/**
 * Make n variations and the pick of one, against a stub backend: Try again n
 * times in a row on one region, one after the other, asked about once, and
 * stopped by the first failure; and a pick that is one undo step the backend
 * records, adopted as the chapter's index.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const asked = vi.hoisted(() => ({
  confirm: vi.fn(async () => /** @type {string|null} */ ('g1')),
  guidance: vi.fn(async () => /** @type {any} */ ({ target: 'other', description: 'a bench' })),
  consent: vi.fn(async () => /** @type {any} */ ({ params: { grantNonce: 'n' }, attemptId: 'a' })),
}))
vi.mock('./nanobananaask.svelte.js', () => ({ confirmNanoBanana: asked.confirm }))
vi.mock('./qwenflow.js', () => ({ askLocalGuidance: asked.guidance }))
vi.mock('./cloudflow.svelte.js', () => ({
  requestCloudConsent: asked.consent,
  runCloudJob: async (/** @type {any} */ grant, /** @type {any} */ _where, /** @type {any} */ call) => call(grant.params),
  cloudOutcomeOf: () => null,
}))

import { setBackend } from '../api/backend.js'
import { app } from '../state/app.svelte.js'
import { capabilities } from '../state/capabilities.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { createHistory } from '../model/history.js'
import { makeVariations, pickVariation, runRegionMenuItem, setVariationCount, stepVariation, variationRuns } from './maskactions.svelte.js'
import { nanoBananaEditOut } from './nanobananaedits.svelte.js'

const CHAPTER = 'chapter'
const ID = `${CHAPTER}-p001-r1`

/** @param {string} engine @param {string} [revision] @param {any} [cloud] */
function region(engine = 'nanobanana', revision = 'r0', cloud = undefined) {
  return {
    id: ID,
    pageId: `${CHAPTER}-p001`,
    bbox: { x: 10, y: 10, w: 20, h: 20 },
    source: 'detected',
    outcome: 'cleaned',
    gateSkipCause: null,
    mask: { id: 'm1', legacyPatchRevision: revision, fillMode: 'reconstruct', provenance: { engine, ...(cloud ? { cloud } : {}) }, layer: {} },
  }
}

/** @param {any} target */
function open(target) {
  editor.project = /** @type {any} */ ({ id: 'pr', mode: 'pages' })
  editor.pageIndex = 0
  editor.chapter = /** @type {any} */ ({
    id: CHAPTER,
    review: [],
    pages: [{ id: `${CHAPTER}-p001`, index: 0, width: 700, height: 1000, status: 'cleaned', resident: true, regionCount: 1, regions: [target] }],
  })
  return /** @type {any} */ (editor.chapter.pages[0].regions[0])
}

/** @param {string} engine */
function backend(engine = 'nanobanana') {
  let made = 0
  const adapter = {
    rerunMask: vi.fn(async () => {
      made += 1
      return { region: region(engine, `r${made}`), pageStatus: 'cleaned' }
    }),
    regionVariations: vi.fn(async () => [
      { revision: 'r0', engine, model: null, created: 1, current: false },
      { revision: 'r1', engine, model: null, created: 2, current: true },
      { revision: 'r2', engine, model: null, created: 3, current: false },
    ]),
    pickVariation: vi.fn(async ({ revision }) => ({
      region: region(engine, revision),
      pageStatus: 'cleaned',
      history: { limit: 500, cursor: 1, entries: [{ seq: 7, label: 'masks.command.pickVariation', page: 0, undone: false }] },
    })),
    loadPages: vi.fn(async () => []),
    historyPush: vi.fn(async () => ({ limit: 500, cursor: 1, entries: [{ seq: 1, label: 'masks.command.rerunMask', page: 0, undone: false }] })),
    historyLoad: vi.fn(async () => ({ limit: 500, cursor: 0, entries: [] })),
  }
  setBackend(/** @type {any} */ (adapter))
  return adapter
}

beforeEach(() => {
  app.notices.length = 0
  editor.history = createHistory()
  editor.run.active = false
  editor.run.busyPages = []
  asked.confirm.mockReset()
  asked.confirm.mockResolvedValue('g1')
  asked.guidance.mockClear()
  asked.consent.mockClear()
  capabilities.nanoBanana = true
  setVariationCount(3)
})

afterEach(() => {
  capabilities.nanoBanana = false
  setBackend(null)
  editor.chapter = null
  editor.project = null
})

describe('Make n variations', () => {
  it('tries a Nano Banana layer again n times in a row, on one window stating n requests', async () => {
    const adapter = backend()
    /** @type {Array<() => void>} */
    const pending = []
    let inFlight = 0
    let most = 0
    adapter.rerunMask.mockImplementation(async () => {
      inFlight += 1
      most = Math.max(most, inFlight)
      await new Promise((resolve) => pending.push(() => resolve(undefined)))
      inFlight -= 1
      return { region: region('nanobanana', `r${pending.length}`), pageStatus: 'cleaned' }
    })
    const target = open(region())
    const run = makeVariations(target, 3)
    for (let at = 1; at <= 3; at += 1) {
      await vi.waitFor(() => expect(adapter.rerunMask).toHaveBeenCalledTimes(at))
      expect(nanoBananaEditOut(ID), 'Stop is on the row the whole time').toBe(true)
      pending[at - 1]()
    }
    expect(await run).toBe(3)
    expect(most, 'one after the other').toBe(1)
    expect(asked.confirm).toHaveBeenCalledTimes(1)
    expect(asked.confirm).toHaveBeenCalledWith(3, CHAPTER)
    for (const call of adapter.rerunMask.mock.calls) {
      expect(call[0]).toMatchObject({ kind: 'retry', params: { nanoBananaGrant: 'g1' } })
    }
    // The run is one undo step, from the result before it to the newest.
    await vi.waitFor(() => expect(adapter.historyPush).toHaveBeenCalledTimes(1))
    const entry = /** @type {any} */ (adapter.historyPush.mock.calls[0])[0].entry
    expect(entry.label).toBe('masks.command.makeVariations')
    expect(entry.before.region.mask.legacyPatchRevision).toBe('r0')
    expect(entry.after.region.mask.legacyPatchRevision).toBe('r3')
    expect(nanoBananaEditOut(ID)).toBe(false)
  })

  it('is one undo step: undo applies the result before the run, redo the newest', async () => {
    const adapter = backend()
    /** @type {any[]} */
    const restored = []
    Object.assign(adapter, {
      restoreRegion: vi.fn(async ({ region: side }) => {
        restored.push(side?.mask?.legacyPatchRevision)
        return side
      }),
      historyMove: vi.fn(async ({ direction }) => {
        const entry = /** @type {any} */ (adapter.historyPush.mock.calls[0])[0].entry
        return { cursor: direction === 'undo' ? 0 : 1, entry: { ...entry, seq: 1 } }
      }),
    })
    expect(await makeVariations(open(region()), 3)).toBe(3)
    await vi.waitFor(() => expect(adapter.historyPush).toHaveBeenCalledTimes(1))
    const { undo, redo } = await import('../state/editor.svelte.js')
    undo()
    await vi.waitFor(() => expect(restored).toEqual(['r0']))
    expect(editor.chapter?.pages[0].regions[0].mask?.legacyPatchRevision).toBe('r0')
    redo()
    await vi.waitFor(() => expect(restored).toEqual(['r0', 'r3']))
    expect(editor.chapter?.pages[0].regions[0].mask?.legacyPatchRevision).toBe('r3')
    expect(adapter.historyPush, 'undo and redo record nothing').toHaveBeenCalledTimes(1)
    expect(adapter.rerunMask, 'and render nothing').toHaveBeenCalledTimes(3)
  })

  it('stopped after 2 of 3 is one step for the two it made', async () => {
    const adapter = backend()
    adapter.rerunMask
      .mockResolvedValueOnce({ region: region('nanobanana', 'r1'), pageStatus: 'cleaned' })
      .mockResolvedValueOnce({ region: region('nanobanana', 'r2'), pageStatus: 'cleaned' })
      .mockResolvedValueOnce(/** @type {any} */ (null))
    expect(await makeVariations(open(region()), 3)).toBe(2)
    await vi.waitFor(() => expect(adapter.historyPush).toHaveBeenCalledTimes(1))
    const entry = /** @type {any} */ (adapter.historyPush.mock.calls[0])[0].entry
    expect(entry.label).toBe('masks.command.makeVariations')
    expect([entry.before.region.mask.legacyPatchRevision, entry.after.region.mask.legacyPatchRevision]).toEqual(['r0', 'r2'])
  })

  it('records nothing when it made none', async () => {
    const adapter = backend()
    adapter.rerunMask.mockResolvedValue(/** @type {any} */ (null))
    expect(await makeVariations(open(region()), 3)).toBe(0)
    expect(adapter.historyPush).not.toHaveBeenCalled()
    expect(editor.history.entries).toEqual([])
  })

  it('stops at the first failure, keeps what it made, and says how many', async () => {
    const adapter = backend()
    adapter.rerunMask
      .mockResolvedValueOnce({ region: region('nanobanana', 'r1'), pageStatus: 'cleaned' })
      .mockResolvedValueOnce(/** @type {any} */ (null))
    expect(await makeVariations(open(region()), 4)).toBe(1)
    expect(adapter.rerunMask).toHaveBeenCalledTimes(2)
    expect(editor.chapter?.pages[0].regions[0].mask?.legacyPatchRevision).toBe('r1')
    expect(app.notices.map((notice) => notice.key)).toContain('notice.variations.stopped')
    expect(app.notices.find((notice) => notice.key === 'notice.variations.stopped')?.params).toEqual({ made: 1, count: 4 })
  })

  it('sends nothing when the window is cancelled', async () => {
    const adapter = backend()
    asked.confirm.mockResolvedValue(null)
    expect(await makeVariations(open(region()), 3)).toBe(0)
    expect(adapter.rerunMask).not.toHaveBeenCalled()
  })

  it('asks a local FLUX layer\'s guidance once and sends it with every one', async () => {
    const adapter = backend('flux')
    expect(await makeVariations(open(region('flux')), 2)).toBe(2)
    expect(asked.guidance).toHaveBeenCalledTimes(1)
    expect(asked.confirm).not.toHaveBeenCalled()
    for (const call of adapter.rerunMask.mock.calls) {
      expect(call[0]).toMatchObject({ kind: 'retry', params: { qwenEdit: { target: 'other', description: 'a bench' } } })
    }
  })

  it('asks a cloud layer\'s guidance once: every consent shares one answer', async () => {
    const cloud = { provider: 'modal', request_id: 'q', attempt_id: 'a0' }
    const adapter = backend('flux')
    adapter.rerunMask.mockImplementation(async () => ({ region: region('flux', `r${adapter.rerunMask.mock.calls.length}`, cloud), pageStatus: 'cleaned' }))
    expect(await makeVariations(open(region('flux', 'r0', cloud)), 3)).toBe(3)
    expect(asked.consent).toHaveBeenCalledTimes(3)
    const holders = asked.consent.mock.calls.map((call) => /** @type {any} */ (call)[2]?.guidance)
    expect(holders.every((holder) => holder && holder === holders[0])).toBe(true)
    expect(adapter.rerunMask.mock.calls.every((call) => /** @type {any} */ (call)[0].engine === 'cloud')).toBe(true)
    // Three cloud renders, one step for the run.
    await vi.waitFor(() => expect(adapter.historyPush).toHaveBeenCalledTimes(1))
    expect(/** @type {any} */ (adapter.historyPush.mock.calls[0])[0].entry.label).toBe('masks.command.makeVariations')
  })

  it('a single cloud Try again is one step of its own', async () => {
    const cloud = { provider: 'modal', request_id: 'q', attempt_id: 'a0' }
    const adapter = backend('flux')
    adapter.rerunMask.mockImplementation(async () => ({ region: region('flux', 'r1', cloud), pageStatus: 'cleaned' }))
    expect(await runRegionMenuItem('retry', open(region('flux', 'r0', cloud)))).toBe(true)
    await vi.waitFor(() => expect(adapter.historyPush).toHaveBeenCalledTimes(1))
    expect(/** @type {any} */ (adapter.historyPush.mock.calls[0])[0].entry.label).toBe('masks.command.rerunMask')
  })

  it('makes none for a LaMa layer, and none on a page a run holds', async () => {
    const adapter = backend('lama')
    expect(await makeVariations(open(region('lama')), 3)).toBe(0)
    editor.run.active = true
    editor.run.busyPages = [0]
    expect(await makeVariations(open(region('nanobanana')), 3)).toBe(0)
    expect(asked.confirm).not.toHaveBeenCalled()
    expect(adapter.rerunMask).not.toHaveBeenCalled()
  })

  it('is the menu entry, at the count the menu remembers', async () => {
    const adapter = backend()
    setVariationCount(2)
    expect(variationRuns.count).toBe(2)
    expect(await runRegionMenuItem('makeVariations', open(region()))).toBe(true)
    expect(adapter.rerunMask).toHaveBeenCalledTimes(2)
    setVariationCount(40)
    expect(variationRuns.count).toBe(8)
  })
})

describe('picking a variation', () => {
  it('is one undo step the backend records, adopted as the index', async () => {
    const adapter = backend()
    expect(await pickVariation(open(region('nanobanana', 'r1')), 'r0')).toBe(true)
    expect(adapter.pickVariation).toHaveBeenCalledWith({ regionId: ID, revision: 'r0' })
    expect(editor.chapter?.pages[0].regions[0].mask?.legacyPatchRevision).toBe('r0')
    expect(editor.history.entries.map((entry) => entry.label)).toEqual(['masks.command.pickVariation'])
    expect(adapter.historyPush, 'nothing recorded here').not.toHaveBeenCalled()
  })

  it('steps to the next and previous one, round the ends', async () => {
    const adapter = backend()
    expect(await stepVariation(open(region('nanobanana', 'r1')), 1)).toBe(true)
    expect(adapter.pickVariation).toHaveBeenLastCalledWith({ regionId: ID, revision: 'r2' })
    adapter.regionVariations.mockResolvedValueOnce([
      { revision: 'r0', engine: 'nanobanana', model: null, created: 1, current: true },
      { revision: 'r1', engine: 'nanobanana', model: null, created: 2, current: false },
    ])
    expect(await stepVariation(open(region('nanobanana', 'r0')), -1)).toBe(true)
    expect(adapter.pickVariation).toHaveBeenLastCalledWith({ regionId: ID, revision: 'r1' })
  })

  it('records nothing for the one it shows already', async () => {
    const adapter = backend()
    adapter.pickVariation.mockResolvedValueOnce({ region: region('nanobanana', 'r1'), pageStatus: 'cleaned', history: /** @type {any} */ (null) })
    expect(await pickVariation(open(region('nanobanana', 'r1')), 'r1')).toBe(false)
    expect(editor.history.entries).toEqual([])
  })
})
