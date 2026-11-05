/**
 * Re-running a Nano Banana layer is a paid request to Google, so the window
 * comes first: for naming it, and for Try again and Try again wider on one of
 * its layers, and for Clean anyway and a detection's clean that name it.
 * Stronger and Simpler land on this computer and ask nothing. A press that
 * would do nothing asks nothing, and neither does one that cannot run here
 * (no key, or Cloud engines off): it says why instead.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const asked = vi.hoisted(() => ({ confirm: vi.fn(async () => true) }))
vi.mock('./nanobananaask.svelte.js', () => ({ confirmNanoBanana: asked.confirm }))

import { setBackend } from '../api/backend.js'
import { app } from '../state/app.svelte.js'
import { capabilities } from '../state/capabilities.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { session } from '../state/session.svelte.js'
import { createHistory } from '../model/history.js'
import { cleanAnyway, cleanDetected, rerunMask, rerunTarget, runRegionMenuItem } from './maskactions.svelte.js'

const CHAPTER = 'chapter'

/** @param {string} engine @param {string} [outcome] */
function region(engine = 'nanobanana', outcome = 'cleaned') {
  return {
    id: `${CHAPTER}-p001-r1`,
    pageId: `${CHAPTER}-p001`,
    bbox: { x: 10, y: 10, w: 20, h: 20 },
    source: 'detected',
    outcome,
    gateSkipCause: null,
    mask: { id: 'm1', fillMode: 'reconstruct', provenance: { engine }, layer: {} },
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

function backend() {
  const adapter = {
    rerunMask: vi.fn(async () => ({ status: 'applied', region: region(), pageStatus: 'cleaned' })),
    cleanAnyway: vi.fn(async () => ({ region: region('nanobanana'), pageStatus: 'cleaned' })),
    applyTool: vi.fn(async () => ({ status: 'applied', region: region('nanobanana'), pageStatus: 'cleaned' })),
    loadPages: vi.fn(async () => []),
    historyPush: vi.fn(async () => ({ cursor: 1, entries: [{ seq: 1, label: 'x' }] })),
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
  asked.confirm.mockResolvedValue(true)
  capabilities.nanoBanana = true
})

afterEach(() => {
  capabilities.nanoBanana = false
  setBackend(null)
  editor.chapter = null
  editor.project = null
})

describe('re-running a Nano Banana layer', () => {
  for (const kind of /** @type {const} */ (['retry', 'retryWider'])) {
    it(`${kind} asks first and sends nothing when the answer is no`, async () => {
      const adapter = backend()
      asked.confirm.mockResolvedValue(false)
      expect(await rerunMask(open(region()), kind)).toBe(false)
      expect(asked.confirm).toHaveBeenCalledWith(1)
      expect(adapter.rerunMask).not.toHaveBeenCalled()
    })

    it(`${kind} runs once the window is confirmed`, async () => {
      const adapter = backend()
      expect(await rerunMask(open(region()), kind)).toBe(true)
      expect(asked.confirm).toHaveBeenCalledTimes(1)
      expect(adapter.rerunMask).toHaveBeenCalledWith(expect.objectContaining({ kind }))
    })
  }

  it('asks when Nano Banana is named on any layer', async () => {
    const adapter = backend()
    asked.confirm.mockResolvedValue(false)
    expect(await rerunMask(open(region('lama')), 'engine', 'nanobanana')).toBe(false)
    expect(asked.confirm).toHaveBeenCalledTimes(1)
    expect(adapter.rerunMask).not.toHaveBeenCalled()
  })

  it('asks nothing for Stronger, Simpler, another engine or a local layer', async () => {
    const adapter = backend()
    expect(await rerunMask(open(region()), 'stronger')).toBe(true)
    expect(await rerunMask(open(region()), 'simpler')).toBe(true)
    expect(await rerunMask(open(region()), 'engine', 'lama')).toBe(true)
    expect(await rerunMask(open(region('lama')), 'retry')).toBe(true)
    expect(asked.confirm).not.toHaveBeenCalled()
    expect(adapter.rerunMask).toHaveBeenCalledTimes(4)
  })

  it('asks nothing on a page a run is working on', async () => {
    const adapter = backend()
    editor.run.active = true
    editor.run.busyPages = [0]
    expect(await rerunMask(open(region()), 'retry')).toBe(false)
    expect(asked.confirm).not.toHaveBeenCalled()
    expect(adapter.rerunMask).not.toHaveBeenCalled()
  })

  it('opens one window for two presses', async () => {
    const adapter = backend()
    /** @type {(value: boolean) => void} */
    let answer = () => {}
    asked.confirm.mockImplementation(() => new Promise((resolve) => { answer = resolve }))
    const target = open(region())
    const first = rerunMask(target, 'retry')
    await vi.waitFor(() => expect(asked.confirm).toHaveBeenCalledTimes(1))
    expect(await rerunMask(target, 'retry')).toBe(false)
    answer(true)
    expect(await first).toBe(true)
    expect(asked.confirm).toHaveBeenCalledTimes(1)
    expect(adapter.rerunMask).toHaveBeenCalledTimes(1)
  })

  for (const kind of /** @type {const} */ (['retry', 'retryWider'])) {
    it(`${kind} opens no window when Nano Banana cannot run here, and says why`, async () => {
      const adapter = backend()
      const cloudWas = session.cloudAllowed
      capabilities.nanoBanana = false
      try {
        session.cloudAllowed = true
        expect(await rerunMask(open(region()), kind)).toBe(false)
        session.cloudAllowed = false
        expect(await rerunMask(open(region()), kind)).toBe(false)
      } finally {
        session.cloudAllowed = cloudWas
      }
      expect(asked.confirm).not.toHaveBeenCalled()
      expect(adapter.rerunMask).not.toHaveBeenCalled()
      expect(app.notices.map((notice) => notice.key)).toEqual(['nanoBanana.target.noKey', 'nanoBanana.target.cloudOff'])
    })
  }
})

describe('stepping off a Nano Banana layer', () => {
  it('lands on the strongest rung this machine runs, as the native side picks it', () => {
    const engines = capabilities.engines
    const mask = /** @type {any} */ (region().mask)
    try {
      capabilities.engines = { ...engines, flux: false }
      expect([rerunTarget(mask, 'stronger'), rerunTarget(mask, 'simpler')]).toEqual(['lama', 'lama'])
      capabilities.engines = { ...engines, flux: true }
      expect([rerunTarget(mask, 'stronger'), rerunTarget(mask, 'simpler')]).toEqual(['flux', 'flux'])
    } finally {
      capabilities.engines = engines
    }
  })
})

describe('Clean anyway on a detection', () => {
  it('records no undo step of its own: the native commit recorded it', async () => {
    const adapter = backend()
    expect(await cleanAnyway(open(region('fill', 'detected')), 'nanobanana')).toBe(true)
    expect(asked.confirm).toHaveBeenCalledTimes(1)
    expect(adapter.cleanAnyway).toHaveBeenCalledTimes(1)
    expect(adapter.historyPush).not.toHaveBeenCalled()
    expect(editor.history.entries).toHaveLength(0)
  })
})

describe('cleaning one region with Nano Banana by name', () => {
  it('Clean anyway asks first, and sends nothing on a no', async () => {
    const adapter = backend()
    asked.confirm.mockResolvedValue(false)
    expect(await cleanAnyway(open(region('fill', 'gate-skipped')), 'nanobanana')).toBe(false)
    expect(asked.confirm).toHaveBeenCalledWith(1)
    expect(adapter.cleanAnyway).not.toHaveBeenCalled()
  })

  it('a detection\'s clean asks first, and sends nothing on a no', async () => {
    const adapter = backend()
    asked.confirm.mockResolvedValue(false)
    expect(await cleanDetected(open(region('fill', 'detected')), 'nanobanana')).toBe(false)
    expect(asked.confirm).toHaveBeenCalledWith(1)
    expect(adapter.applyTool).not.toHaveBeenCalled()
    asked.confirm.mockResolvedValue(true)
    expect(await cleanDetected(open(region('fill', 'detected')), 'nanobanana')).toBe(true)
    expect(adapter.applyTool).toHaveBeenCalledWith(expect.objectContaining({ params: expect.objectContaining({ engine: 'nanobanana' }) }))
  })

  it('asks once from the region menu, not twice', async () => {
    const adapter = backend()
    expect(await runRegionMenuItem('approve:nanobanana', open(region('fill', 'gate-skipped')))).toBe(true)
    expect(await runRegionMenuItem('engine:nanobanana', open(region('fill', 'detected')))).toBe(true)
    expect(asked.confirm).toHaveBeenCalledTimes(2)
    expect(adapter.cleanAnyway).toHaveBeenCalledTimes(1)
    expect(adapter.applyTool).toHaveBeenCalledTimes(1)
  })

  it('asks nothing for a local engine', async () => {
    const adapter = backend()
    expect(await cleanAnyway(open(region('fill', 'gate-skipped')), 'lama')).toBe(true)
    expect(await cleanDetected(open(region('fill', 'detected')), 'lama')).toBe(true)
    expect(asked.confirm).not.toHaveBeenCalled()
    expect(adapter.cleanAnyway).toHaveBeenCalledTimes(1)
    expect(adapter.applyTool).toHaveBeenCalledTimes(1)
  })

  it('opens no window when Nano Banana cannot run here, and says why', async () => {
    const adapter = backend()
    capabilities.nanoBanana = false
    expect(await cleanAnyway(open(region('fill', 'gate-skipped')), 'nanobanana')).toBe(false)
    expect(await cleanDetected(open(region('fill', 'detected')), 'nanobanana')).toBe(false)
    expect(asked.confirm).not.toHaveBeenCalled()
    expect(adapter.cleanAnyway).not.toHaveBeenCalled()
    expect(adapter.applyTool).not.toHaveBeenCalled()
    expect(app.notices.map((notice) => notice.key)).toHaveLength(2)
  })
})
