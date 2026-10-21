/**
 * A local re-run against a stub backend whose answer the case controls.
 *
 * On a longstrip a region that hangs past the bottom of its page is drawn by
 * the next page too, so a re-run reads both pages again. A second press while
 * the first is rendering is not a second render.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setBackend } from '../api/backend.js'
import { app } from '../state/app.svelte.js'
import { editor } from '../state/editor.svelte.js'
import { createHistory } from '../model/history.js'
import { rerunMask } from './maskactions.svelte.js'

const CHAPTER = 'strip'

/**
 * @param {string} maskId
 * @param {{x: number, y: number, w: number, h: number}} bbox
 */
function region(maskId = 'm1', bbox = { x: 10, y: 80, w: 20, h: 40 }) {
  return {
    id: 'strip-p002-r1',
    pageId: 'strip-p002',
    // 80% down a 1000 px page and 40% tall: the last 20% is on page 3.
    bbox,
    source: 'detected',
    outcome: 'cleaned',
    gateSkipCause: null,
    mask: { id: maskId, fillMode: 'match-surround', provenance: { engine: 'fill' }, layer: {} },
  }
}

/** Five resident 1000 px pages; the second holds the overhanging region. */
/** @param {{x: number, y: number, w: number, h: number}} [bbox] */
function open(bbox) {
  editor.project = /** @type {any} */ ({ id: 'pr', mode: 'longstrip' })
  editor.pageIndex = 1
  editor.chapter = /** @type {any} */ ({
    id: CHAPTER,
    review: [],
    pages: Array.from({ length: 5 }, (_, index) => ({
      id: `${CHAPTER}-p00${index + 1}`,
      index,
      width: 700,
      height: 1000,
      status: 'cleaned',
      resident: true,
      regionCount: index === 1 ? 1 : 0,
      regions: index === 1 ? [region('m1', bbox)] : [],
    })),
  })
  return /** @type {any} */ (editor.chapter.pages[1].regions[0])
}

/** A promise and the hand that settles it. */
function deferred() {
  /** @type {(value: any) => void} */
  let resolve = () => {}
  const promise = new Promise((settle) => { resolve = settle })
  return { promise, resolve }
}

/** @param {() => Promise<any>} answer */
function backend(answer) {
  const adapter = {
    rerunMask: vi.fn(answer),
    loadPages: vi.fn(async () => []),
    historyPush: vi.fn(async () => ({ cursor: 1, entries: [{ seq: 1, label: 'x' }] })),
  }
  setBackend(/** @type {any} */ (adapter))
  return adapter
}

const applied = () => ({ status: 'applied', region: region('m2'), pageStatus: 'cleaned' })

beforeEach(() => {
  app.notices.length = 0
  editor.history = createHistory()
  editor.run.active = false
  editor.run.busyPages = []
})

afterEach(() => {
  setBackend(null)
  editor.chapter = null
  editor.project = null
  editor.pageIndex = 0
})

describe('rerunMask on a longstrip', () => {
  it('reads the region\'s page and the neighbour it hangs onto again', async () => {
    const adapter = backend(async () => applied())
    expect(await rerunMask(open(), 'stronger')).toBe(true)
    await vi.waitFor(() => expect(adapter.loadPages).toHaveBeenCalledTimes(2))
    const asked = adapter.loadPages.mock.calls.flatMap(([call]) => call.indices).sort()
    expect(asked).toEqual([1, 2])
    expect(adapter.loadPages.mock.calls.every(([call]) => call.chapterId === CHAPTER)).toBe(true)
  })

  it('reads only its own page when the region stays inside it', async () => {
    const inside = { x: 10, y: 10, w: 20, h: 20 }
    const adapter = backend(async () => ({ ...applied(), region: region('m2', inside) }))
    expect(await rerunMask(open(inside), 'stronger')).toBe(true)
    await vi.waitFor(() => expect(adapter.loadPages).toHaveBeenCalledTimes(1))
    expect(adapter.loadPages).toHaveBeenCalledWith({ chapterId: CHAPTER, indices: [1] })
  })
})

describe('rerunMask pressed twice', () => {
  it('runs once, answers the second press false, and runs again after the first settles', async () => {
    const first = deferred()
    const adapter = backend(() => first.promise)
    const target = open()

    const one = rerunMask(target, 'stronger')
    await vi.waitFor(() => expect(adapter.rerunMask).toHaveBeenCalledTimes(1))
    expect(await rerunMask(target, 'stronger')).toBe(false)
    expect(adapter.rerunMask).toHaveBeenCalledTimes(1)

    first.resolve(applied())
    expect(await one).toBe(true)
    expect(editor.history.entries).toHaveLength(1)

    adapter.rerunMask.mockResolvedValueOnce(applied())
    expect(await rerunMask(target, 'stronger')).toBe(true)
    expect(adapter.rerunMask).toHaveBeenCalledTimes(2)
  })

  it('lets the next press through after a rejected run', async () => {
    const adapter = backend(async () => { throw new Error('engine fault') })
    vi.spyOn(console, 'error').mockImplementation(() => {})
    const target = open()
    expect(await rerunMask(target, 'stronger')).toBe(false)
    adapter.rerunMask.mockResolvedValueOnce(applied())
    expect(await rerunMask(target, 'stronger')).toBe(true)
    expect(adapter.rerunMask).toHaveBeenCalledTimes(2)
  })
})
