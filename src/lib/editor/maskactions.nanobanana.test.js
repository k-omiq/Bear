/**
 * Re-running a Nano Banana layer is a paid request to Google, so the window
 * comes first: for naming it, and for Try again and Try again wider on one of
 * its layers. Stronger and Simpler land on this computer and ask nothing. A
 * press that would do nothing asks nothing.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const asked = vi.hoisted(() => ({ confirm: vi.fn(async () => true) }))
vi.mock('./nanobananaask.svelte.js', () => ({ confirmNanoBanana: asked.confirm }))

import { setBackend } from '../api/backend.js'
import { app } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { createHistory } from '../model/history.js'
import { cleanAnyway, rerunMask } from './maskactions.svelte.js'

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
})

afterEach(() => {
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
})

describe('Clean anyway on a detection', () => {
  it('records no undo step of its own: the native commit recorded it', async () => {
    const adapter = backend()
    expect(await cleanAnyway(open(region('fill', 'detected')), 'nanobanana')).toBe(true)
    expect(adapter.cleanAnyway).toHaveBeenCalledTimes(1)
    expect(adapter.historyPush).not.toHaveBeenCalled()
    expect(editor.history.entries).toHaveLength(0)
  })
})
