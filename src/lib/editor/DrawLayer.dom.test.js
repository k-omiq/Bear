/**
 * The mounted drawing surface across a long-strip page seam.
 *
 * Geometry unit tests pin the arithmetic, while this test pins the event path:
 * browser pointer coordinates → DrawLayer draft → drawing commit → backend
 * payload and the layer each page it reaches draws.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { setBackend } from '../api/backend.js'
import { createHistory } from '../model/history.js'
import { app, clearNotices } from '../state/app.svelte.js'
import { editor, undo } from '../state/editor.svelte.js'
import { session, setMaskColor, setOutsideMaskColor } from '../state/session.svelte.js'
import { draft, resetDraftState } from './draft.svelte.js'
import { pageLayers } from './patchlayers.svelte.js'
import DrawLayer from './DrawLayer.svelte'
import { radiusAt } from './paintplan.js'

function page(id, index) {
  return {
    id,
    chapterId: 'c1',
    index,
    number: index + 1,
    status: 'unclean',
    width: 800,
    height: 1000,
    regionCount: 0,
    resident: true,
    regions: [],
  }
}

afterEach(() => {
  cleanup()
  setBackend(null)
  editor.chapter = null
  editor.project = null
  editor.tool = 'autoClean'
  editor.areaRuns = []
  resetDraftState()
  clearNotices()
  vi.clearAllMocks()
})

describe('brush pressure on a mounted surface', () => {
  it.each([
    ['mouse', 0.5, 0.5, 1, 1],
    ['touch', 0.5, 0.5, 1, 1],
    ['pen', 0.25, 0.75, 0.25, 0.75],
  ])('commits the intended brush footprint for %s input', async (pointerType, downPressure, movePressure, downExpected, moveExpected) => {
    const only = page('c1-p001', 0)
    editor.chapter = { id: 'c1', review: [], pages: [only] }
    editor.project = /** @type {any} */ ({ mode: 'single' })
    editor.history = createHistory()
    editor.pageIndex = 0
    editor.tool = 'brush'
    editor.toolParams = { brush: { mode: 'paint', size: 80 } }
    const createRegion = vi.fn(async ({ bbox }) => ({
      region: { id: 'c1-p001-h1', pageId: only.id, bbox, source: 'hand', outcome: 'cleaned' },
      pageStatus: 'cleaned',
    }))
    setBackend(/** @type {any} */ ({
      createRegion,
      historyPush: vi.fn().mockResolvedValue({ cursor: 1, entries: [] }),
    }))
    // This test checks the stroke payload; pixel rendering needs a real canvas.
    const canvas = vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null)
    try {
      const view = render(DrawLayer, { props: { page: only } })
      const surface = view.getByRole('button')
      surface.getBoundingClientRect = () => /** @type {DOMRect} */ ({
        x: 0, y: 0, left: 0, top: 0, right: 100, bottom: 100, width: 100, height: 100, toJSON() {},
      })
      surface.setPointerCapture = vi.fn()
      surface.releasePointerCapture = vi.fn()
      await fireEvent.pointerDown(surface, { pointerId: 1, pointerType, pressure: downPressure, button: 0, clientX: 20, clientY: 20 })
      await fireEvent.pointerMove(surface, { pointerId: 1, pointerType, pressure: movePressure, buttons: 1, clientX: 60, clientY: 20 })
      const ring = view.container.querySelector('svg.cursor ellipse')
      const footprintRadius = Number(ring?.getAttribute('rx')) / 100 * only.width
      expect(footprintRadius).toBe(40)
      await fireEvent.pointerUp(surface, { pointerId: 1, pointerType, pressure: 0, button: 0, clientX: 60, clientY: 20 })
      await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(1))
      const { paint } = createRegion.mock.calls[0][0].params
      // The rope leaves the move's point 3 px short of the pointer, and the
      // release ends the stroke where the pointer let go, at the pressure it
      // was drawn with: a pen lifts at 0.
      expect(paint.points.map((point) => point.x)).toEqual([20, 57, 60])
      expect(paint.points.map((point) => point.p)).toEqual([downExpected, moveExpected, moveExpected])
      expect(radiusAt(paint, paint.points[0].p)).toBe(footprintRadius * downExpected)
      expect(radiusAt(paint, paint.points[1].p)).toBe(footprintRadius * moveExpected)
      await editor.history.running
    } finally {
      cleanup()
      canvas.mockRestore()
    }
  })
})

// A run holds the page it is on and no other (`editor.svelte.js#pageBusy`): a
// gesture on that page is not begun, and says why; the same gesture on any
// other page of the chapter is drawn and sent while the run goes on.
describe('a stroke while a run is going', () => {
  it('accepts additional detection rectangles by pointer and keyboard while the same page is detecting', async () => {
    const only = page('c1-p001', 0)
    editor.chapter = { id: 'c1', review: [], pages: [only] }
    editor.project = /** @type {any} */ ({ mode: 'single' })
    editor.pageIndex = 0
    editor.run.active = false
    editor.tool = 'maskSelect'
    editor.toolParams = { maskSelect: { mode: 'detect', shape: 'rect' } }
    const previousTargets = session.analysisTargets
    session.analysisTargets = { rtFull: 'local', samTs: 'local' }
    let count = 0
    const runClean = vi.fn(async () => ({ runId: `area-${++count}`, pages: [{ pageIndex: 0 }] }))
    setBackend(/** @type {any} */ ({ runClean }))
    try {
      const view = render(DrawLayer, { props: { page: only } })
      const surface = view.getByRole('button')
      surface.getBoundingClientRect = () => /** @type {DOMRect} */ ({
        x: 0, y: 0, left: 0, top: 0, right: 100, bottom: 100, width: 100, height: 100, toJSON() {},
      })
      surface.setPointerCapture = vi.fn()
      surface.releasePointerCapture = vi.fn()
      for (const x of [10, 50]) {
        await fireEvent.pointerDown(surface, { pointerId: 1, pointerType: 'mouse', button: 0, clientX: x, clientY: 10 })
        await fireEvent.pointerMove(surface, { pointerId: 1, pointerType: 'mouse', buttons: 1, clientX: x + 20, clientY: 30 })
        await fireEvent.pointerUp(surface, { pointerId: 1, pointerType: 'mouse', button: 0, clientX: x + 20, clientY: 30 })
        await waitFor(() => expect(runClean).toHaveBeenCalledTimes(x === 10 ? 1 : 2))
      }
      expect(editor.areaRuns).toHaveLength(2)
      await fireEvent.keyDown(surface, { key: 'Enter' })
      expect(draft.active?.mode).toBe('detect')
      await fireEvent.keyDown(surface, { key: 'Enter' })
      await waitFor(() => expect(runClean).toHaveBeenCalledTimes(3))
      expect(app.notices).toEqual([])
      expect(editor.areaRuns).toHaveLength(3)
    } finally {
      session.analysisTargets = previousTargets
    }
  })

  it('is refused on the page the run is on, and drawn on another', async () => {
    const busy = page('c1-p001', 0)
    const free = page('c1-p002', 1)
    editor.chapter = { id: 'c1', review: [], pages: [busy, free] }
    editor.project = /** @type {any} */ ({ mode: 'single' })
    editor.history = createHistory()
    editor.tool = 'brush'
    editor.toolParams = { brush: { mode: 'paint', size: 80 } }
    editor.run.active = true
    editor.run.busyPages = [0]
    const createRegion = vi.fn(async ({ bbox, pageIndex }) => ({
      region: { id: `c1-p00${pageIndex + 1}-h1`, pageId: pageIndex ? free.id : busy.id, bbox, source: 'hand', outcome: 'cleaned' },
      pageStatus: 'cleaned',
    }))
    setBackend(/** @type {any} */ ({
      createRegion,
      historyPush: vi.fn().mockResolvedValue({ cursor: 1, entries: [] }),
    }))
    const canvas = vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null)
    /** @param {any} on */
    const stroke = async (on) => {
      const view = render(DrawLayer, { props: { page: on } })
      const surface = view.getByRole('button')
      surface.getBoundingClientRect = () => /** @type {DOMRect} */ ({
        x: 0, y: 0, left: 0, top: 0, right: 100, bottom: 100, width: 100, height: 100, toJSON() {},
      })
      surface.setPointerCapture = vi.fn()
      surface.releasePointerCapture = vi.fn()
      await fireEvent.pointerDown(surface, { pointerId: 1, pointerType: 'mouse', button: 0, clientX: 20, clientY: 20 })
      await fireEvent.pointerMove(surface, { pointerId: 1, pointerType: 'mouse', buttons: 1, clientX: 60, clientY: 20 })
      await fireEvent.pointerUp(surface, { pointerId: 1, pointerType: 'mouse', button: 0, clientX: 60, clientY: 20 })
      return view
    }
    try {
      editor.pageIndex = 0
      await stroke(busy)
      expect(draft.active).toBeFalsy()
      expect(app.notices.at(-1)?.key).toBe('notice.run.pageBusy')
      expect(createRegion).not.toHaveBeenCalled()
      cleanup()

      // The keyboard's gesture asks the same question: no draft opens.
      const said = app.notices.length
      const keyed = render(DrawLayer, { props: { page: busy } })
      await fireEvent.keyDown(keyed.getByRole('button'), { key: 'Enter' })
      expect(draft.active).toBeFalsy()
      expect(app.notices).toHaveLength(said + 1)
      expect(app.notices.at(-1)?.key).toBe('notice.run.pageBusy')
      cleanup()

      editor.pageIndex = 1
      // And on a page the run is not on, the same key opens one.
      const open = render(DrawLayer, { props: { page: free } })
      await fireEvent.keyDown(open.getByRole('button'), { key: 'Enter' })
      expect(draft.active).toBeTruthy()
      expect(app.notices).toHaveLength(said + 1)
      cleanup()
      resetDraftState()

      await stroke(free)
      await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(1))
      expect(createRegion.mock.calls[0][0].pageIndex).toBe(1)
      await editor.history.running
    } finally {
      cleanup()
      canvas.mockRestore()
      editor.run.active = false
      editor.run.busyPages = []
    }
  })
})

/**
 * **The clone stamp, driven the way a raster editor's is.** The sheet here is
 * 100 by 100 client pixels, so a client coordinate is a page percent.
 */
describe('the clone stamp on a mounted surface', () => {
  /** @param {Record<string, unknown>} [params] */
  function mount(params = {}) {
    const only = page('c1-p001', 0)
    editor.chapter = { id: 'c1', review: [], pages: [only] }
    editor.project = /** @type {any} */ ({ mode: 'single' })
    editor.history = createHistory()
    editor.pageIndex = 0
    editor.tool = 'cloneStamp'
    editor.toolParams = {
      cloneStamp: { size: 80, hardness: 50, spacing: 12, opacity: 100, flow: 100, alignment: 'aligned', ...params },
    }
    resetDraftState()
    let made = 0
    const createRegion = vi.fn(async ({ bbox }) => {
      made += 1
      return {
        region: { id: `c1-p001-h${made}`, pageId: only.id, bbox, source: 'hand', outcome: 'cleaned' },
        pageStatus: 'cleaned',
      }
    })
    setBackend(/** @type {any} */ ({
      createRegion,
      historyPush: vi.fn().mockResolvedValue({ cursor: 1, entries: [] }),
    }))
    // These tests check what is sent; the pixels need a real canvas.
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null)
    const view = render(DrawLayer, { props: { page: only } })
    const surface = view.getByRole('button')
    surface.getBoundingClientRect = () => /** @type {DOMRect} */ ({
      x: 0, y: 0, left: 0, top: 0, right: 100, bottom: 100, width: 100, height: 100, toJSON() {},
    })
    surface.setPointerCapture = vi.fn()
    surface.releasePointerCapture = vi.fn()
    /** A press and a release in one place. */
    const click = async (x, y, keys = {}) => {
      await fireEvent.pointerDown(surface, { pointerId: 1, button: 0, clientX: x, clientY: y, ...keys })
      await fireEvent.pointerUp(surface, { pointerId: 1, button: 0, clientX: x, clientY: y, ...keys })
    }
    const sent = (index) => createRegion.mock.calls[index][0]
    return { view, surface, createRegion, click, sent }
  }

  afterEach(() => vi.restoreAllMocks())

  it('asks for a source before it paints anything', async () => {
    const { click, createRegion } = mount()
    await click(20, 30)
    expect(app.notices.map((notice) => notice.key)).toEqual(['notice.tool.cloneNeedsSource'])
    expect(draft.active).toBeNull()
    expect(createRegion).not.toHaveBeenCalled()
  })

  it('samples on the modifier-click, and then one click is one dab', async () => {
    const { click, createRegion, sent } = mount()
    await click(50, 50, { altKey: true })
    expect(draft.cloneSource).toEqual({ pageId: 'c1-p001', x: 50, y: 50 })
    expect(createRegion).not.toHaveBeenCalled()

    await click(20, 30)
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(1))
    expect(sent(0).tool).toBe('cloneStamp')
    expect(sent(0).params.cloneSource).toEqual({ x: 50, y: 50 })
    expect(sent(0).params.cloneOffset).toEqual({ x: 30, y: 20 })
    expect(sent(0).params.paint.points).toEqual([{ x: 20, y: 30, p: 1 }])
    // The dab's own box: an 80 px brush on an 800 by 1000 page.
    expect(sent(0).bbox).toEqual({ x: 15, y: 26, w: 10, h: 8 })
    await editor.history.running
  })

  it('samples with the modifier the user chose, and paints on the old one', async () => {
    const before = session.cloneSourceModifier
    session.cloneSourceModifier = 'meta'
    try {
      const { click, createRegion } = mount()
      await click(50, 50, { altKey: true })
      expect(draft.cloneSource).toBeNull()
      await click(50, 50, { metaKey: true })
      expect(draft.cloneSource).toEqual({ pageId: 'c1-p001', x: 50, y: 50 })
      expect(createRegion).not.toHaveBeenCalled()
    } finally {
      session.cloneSourceModifier = before
    }
  })

  it('aligned reads on from the last stroke: the offset stays', async () => {
    const { click, createRegion, sent } = mount({ alignment: 'aligned' })
    await click(50, 50, { altKey: true })
    await click(20, 30)
    await click(60, 60)
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(2))
    expect(sent(1).params.cloneOffset).toEqual({ x: 30, y: 20 })
    expect(sent(1).params.cloneSource).toEqual({ x: 90, y: 80 })
    await editor.history.running
  })

  it('non-aligned starts every stroke from the sampled point', async () => {
    const { click, createRegion, sent } = mount({ alignment: 'nonAligned' })
    await click(50, 50, { altKey: true })
    await click(20, 30)
    await click(60, 60)
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(2))
    expect(sent(1).params.cloneSource).toEqual({ x: 50, y: 50 })
    expect(sent(1).params.cloneOffset).toEqual({ x: -10, y: -10 })
    await editor.history.running
  })

  it('sampling again starts a new offset, aligned or not', async () => {
    const { click, createRegion, sent } = mount({ alignment: 'aligned' })
    await click(50, 50, { altKey: true })
    await click(20, 30)
    await click(70, 10, { altKey: true })
    await click(60, 60)
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(2))
    expect(sent(1).params.cloneSource).toEqual({ x: 70, y: 10 })
    expect(sent(1).params.cloneOffset).toEqual({ x: 10, y: -50 })
    await editor.history.running
  })

  it('fixes the source when the stroke starts, whatever is sampled after', async () => {
    const { surface, createRegion, sent } = mount()
    await fireEvent.pointerDown(surface, { pointerId: 1, button: 0, clientX: 50, clientY: 50, altKey: true })
    await fireEvent.pointerDown(surface, { pointerId: 1, button: 0, clientX: 20, clientY: 30 })
    await fireEvent.pointerMove(surface, { pointerId: 1, buttons: 1, clientX: 40, clientY: 30 })
    expect(draft.active?.clone).toEqual({ source: { x: 50, y: 50 }, offset: { x: 30, y: 20 } })
    // The cross rides with the brush: where the pointer is, plus the offset.
    const cross = /** @type {HTMLElement} */ (document.querySelector('[data-clone-source]'))
    expect(cross.style.left).toBe('70%')
    expect(cross.style.top).toBe('50%')
    await fireEvent.pointerUp(surface, { pointerId: 1, button: 0, clientX: 40, clientY: 30 })
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(1))
    // The rope trails the pointer by 3 px; the release brings the end home.
    expect(sent(0).params.paint.points.map((point) => point.x)).toEqual([20, 37, 40])
    expect(sent(0).params.cloneOffset).toEqual({ x: 30, y: 20 })
    await editor.history.running
  })

  it('draws a straight line to a Shift-click from where the last stroke ended', async () => {
    const { click, createRegion, sent } = mount()
    await click(50, 50, { altKey: true })
    await click(20, 30)
    await click(40, 70, { shiftKey: true })
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(2))
    expect(sent(1).params.paint.points.map(({ x, y }) => ({ x, y }))).toEqual([{ x: 20, y: 30 }, { x: 40, y: 70 }])
    expect(sent(1).bbox).toEqual({ x: 15, y: 26, w: 30, h: 48 })
    await editor.history.running
  })

  it('holds a drag to one axis while Shift is down', async () => {
    const { surface, createRegion, sent } = mount()
    await fireEvent.pointerDown(surface, { pointerId: 1, button: 0, clientX: 50, clientY: 50, altKey: true })
    await fireEvent.pointerDown(surface, { pointerId: 1, button: 0, clientX: 20, clientY: 20 })
    await fireEvent.pointerMove(surface, { pointerId: 1, buttons: 1, clientX: 30, clientY: 23, shiftKey: true })
    await fireEvent.pointerMove(surface, { pointerId: 1, buttons: 1, clientX: 45, clientY: 29, shiftKey: true })
    await fireEvent.pointerUp(surface, { pointerId: 1, button: 0, clientX: 45, clientY: 29 })
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(1))
    expect(sent(0).params.paint.points.map(({ x, y }) => ({ x, y }))).toEqual([
      { x: 20, y: 20 }, { x: 30, y: 20 }, { x: 45, y: 20 },
    ])
    await editor.history.running
  })

  // A stamp is used in quick taps. Its preview is drawn in the page, so a tap
  // is sent when it ends, with no wait on a native frame.
  it('sends each quick tap when it ends', async () => {
    const { click, createRegion, sent } = mount()
    await click(50, 50, { altKey: true })
    await click(20, 30)
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(1))
    await click(60, 60)
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(2))
    expect(sent(0).params.paint.points[0]).toMatchObject({ x: 20, y: 30 })
    expect(sent(1).params.paint.points[0]).toMatchObject({ x: 60, y: 60 })
    await editor.history.running
  })

  it('shows a target while the sample modifier is down, in place of the brush', async () => {
    const { view, surface } = mount()
    await fireEvent.pointerMove(surface, { clientX: 40, clientY: 40 })
    expect(view.container.querySelector('svg.cursor')).not.toBeNull()
    expect(view.container.querySelector('.target')).toBeNull()

    await fireEvent.keyDown(window, { key: 'Alt', altKey: true })
    expect(view.container.querySelector('svg.cursor')).toBeNull()
    expect(view.container.querySelector('.target')).not.toBeNull()

    await fireEvent.keyUp(window, { key: 'Alt', altKey: false })
    expect(view.container.querySelector('svg.cursor')).not.toBeNull()
    expect(view.container.querySelector('.target')).toBeNull()
  })

  it('marks the source it will read from before the stroke starts', async () => {
    const { view, surface, click } = mount()
    expect(view.container.querySelector('[data-clone-source]')).toBeNull()
    await click(50, 50, { altKey: true })
    // The pointer is where it sampled, so the brush would read from there.
    await fireEvent.pointerMove(surface, { clientX: 50, clientY: 50 })
    const cross = /** @type {HTMLElement} */ (view.container.querySelector('[data-clone-source]'))
    expect(cross.style.left).toBe('50%')
    // The brush fills with what it would copy: a round overlay on the ring.
    await fireEvent.pointerMove(surface, { clientX: 20, clientY: 30 })
    const overlay = /** @type {HTMLElement} */ (view.container.querySelector('canvas.overlay'))
    expect(overlay.style.left).toBe('15%')
    expect(overlay.style.top).toBe('26%')
    expect(overlay.style.width).toBe('10%')
    expect(overlay.style.height).toBe('8%')
  })

  it('steps the brush on the bracket keys and keeps the keys from paging', async () => {
    const { surface } = mount({ size: 20, hardness: 50 })
    const paged = vi.fn()
    window.addEventListener('keydown', paged)
    try {
      await fireEvent.keyDown(surface, { key: ']' })
      expect(editor.toolParams.cloneStamp.size).toBe(30)
      await fireEvent.keyDown(surface, { key: '[' })
      await fireEvent.keyDown(surface, { key: '[' })
      expect(editor.toolParams.cloneStamp.size).toBe(18)
      await fireEvent.keyDown(surface, { key: '}', shiftKey: true })
      expect(editor.toolParams.cloneStamp.hardness).toBe(75)
      await fireEvent.keyDown(surface, { key: '}', shiftKey: true })
      await fireEvent.keyDown(surface, { key: '}', shiftKey: true })
      expect(editor.toolParams.cloneStamp.hardness).toBe(100)
      await fireEvent.keyDown(surface, { key: '{', shiftKey: true })
      expect(editor.toolParams.cloneStamp.hardness).toBe(75)
      expect(paged).not.toHaveBeenCalled()
    } finally {
      window.removeEventListener('keydown', paged)
    }
  })

  it('stamps from the keyboard: S samples, Enter opens a dab, arrows move it, Enter stamps', async () => {
    const { surface, createRegion, sent } = mount()
    await fireEvent.keyDown(surface, { key: 's' })
    expect(draft.cloneSource).toEqual({ pageId: 'c1-p001', x: 50, y: 50 })

    await fireEvent.keyDown(surface, { key: 'Enter' })
    expect(draft.active).toMatchObject({ tool: 'cloneStamp', kind: 'stroke', keyboard: true })
    for (let press = 0; press < 3; press += 1) await fireEvent.keyDown(surface, { key: 'ArrowLeft' })
    await fireEvent.keyDown(surface, { key: 'ArrowUp', shiftKey: true })
    expect(draft.active?.points).toEqual([{ x: 47, y: 45, p: 1 }])

    await fireEvent.keyDown(surface, { key: 'Enter' })
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(1))
    expect(sent(0).tool).toBe('cloneStamp')
    expect(sent(0).params.cloneSource).toEqual({ x: 50, y: 50 })
    expect(sent(0).params.cloneOffset).toEqual({ x: 3, y: 5 })
    expect(sent(0).params.paint.points).toEqual([{ x: 47, y: 45, p: 1 }])
    await editor.history.running
  })

  it('does not stamp from the keyboard before a source is sampled', async () => {
    const { surface, createRegion } = mount()
    await fireEvent.keyDown(surface, { key: 'Enter' })
    await fireEvent.keyDown(surface, { key: 'Enter' })
    expect(app.notices.map((notice) => notice.key)).toEqual(['notice.tool.cloneNeedsSource'])
    expect(createRegion).not.toHaveBeenCalled()
    // The dab stays open: S there samples, and it can then be moved and stamped.
    expect(draft.active?.keyboard).toBe(true)
  })
})

describe('a mounted long-strip drawing surface', () => {
  it('commits a rectangle across the seam without clamping it to the anchor page', async () => {
    const first = page('c1-p001', 0)
    const second = page('c1-p002', 1)
    editor.chapter = { id: 'c1', review: [], pages: [first, second] }
    editor.project = /** @type {any} */ ({ mode: 'longstrip' })
    editor.history = createHistory()
    editor.pageIndex = 0
    editor.tool = 'shapes'
    editor.toolParams = {
      shapes: { shape: 'rect', mode: 'solid', feather: 0, color: '#ffffff', opacity: 100 },
    }
    resetDraftState()

    const createRegion = vi.fn(async ({ bbox }) => ({
      region: {
        id: 'c1-p001-h1',
        pageId: first.id,
        bbox,
        source: 'hand',
        outcome: 'cleaned',
        mask: { id: 'c1-p001-h1-m1', regionId: 'c1-p001-h1', layerKey: 'k1', order: 0 },
      },
      pageStatus: 'cleaned',
    }))
    let journalEntry
    const restoreRegion = vi.fn().mockResolvedValue(null)
    setBackend(/** @type {any} */ ({
      createRegion,
      historyPush: vi.fn().mockImplementation(async ({ entry }) => {
        journalEntry = entry
        return { cursor: 1, entries: [{ seq: 1, label: entry.label }] }
      }),
      historyMove: vi.fn().mockImplementation(async () => ({ entry: journalEntry })),
      restoreRegion,
    }))

    const view = render(DrawLayer, {
      props: { page: first, strip: true, stripMinY: 0, stripMaxY: 200 },
    })
    const surface = view.getByRole('button')
    surface.getBoundingClientRect = () => /** @type {DOMRect} */ ({
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      right: 100,
      bottom: 100,
      width: 100,
      height: 100,
      toJSON() {},
    })
    surface.setPointerCapture = vi.fn()
    surface.releasePointerCapture = vi.fn()

    await fireEvent.pointerDown(surface, { pointerId: 7, button: 0, clientX: 20, clientY: 95 })
    await fireEvent.pointerMove(surface, { pointerId: 7, buttons: 1, clientX: 60, clientY: 112 })
    await fireEvent.pointerUp(surface, { pointerId: 7, button: 0, clientX: 60, clientY: 112 })

    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(1))
    const request = createRegion.mock.calls[0][0]
    expect(request.pageIndex).toBe(0)
    expect(request.bbox).toMatchObject({ x: 20, y: 95, w: 40 })
    expect(request.bbox.h).toBeCloseTo(17)
    expect(request.bbox.y + request.bbox.h).toBeCloseTo(112)
    expect(request.params.painted).toEqual({
      kind: 'rect',
      points: [
        { x: 20, y: 95 },
        { x: 60, y: 95 },
        { x: 60, y: 112 },
        { x: 20, y: 112 },
      ],
      feather: 0,
    })

    // A spanning region is drawn by both pages it reaches: the anchor as its
    // own layer, the neighbour as the part across the join.
    vi.stubGlobal('__TAURI_INTERNALS__', { convertFileSrc: (_path, scheme) => `${scheme}://localhost/` })
    const drawnOn = () => editor.chapter.pages.map((candidate) =>
      pageLayers(candidate, editor.chapter.pages).map((layer) => layer.id))
    await waitFor(() => expect(drawnOn()).toEqual([['c1-p001-h1'], ['c1-p001-h1']]))
    expect(pageLayers(editor.chapter.pages[1], editor.chapter.pages)[0].url).toContain('/1/layer/c1-p001-h1?')

    await editor.history.running
    undo()
    await waitFor(() => expect(restoreRegion).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(drawnOn()).toEqual([[], []]))
    vi.unstubAllGlobals()
  })
})

describe('a polygon and a tap across a long-strip join', () => {
  /** Two pages' surfaces, stacked: the first at 0..100px, the second at 100..200px. */
  function mountPair(tool, toolParams, firstRegions = []) {
    const first = { ...page('c1-p001', 0), regions: firstRegions }
    const second = page('c1-p002', 1)
    editor.chapter = { id: 'c1', review: [], pages: [first, second] }
    editor.project = /** @type {any} */ ({ mode: 'longstrip' })
    editor.history = createHistory()
    editor.pageIndex = 0
    editor.tool = tool
    editor.toolParams = toolParams
    editor.selectionId = null
    resetDraftState()
    setBackend(/** @type {any} */ ({ createRegion: vi.fn(async () => null) }))
    const surfaces = [first, second].map((sheet, index) => {
      const view = render(DrawLayer, {
        props: {
          page: sheet,
          strip: true,
          stripMinY: index === 0 ? 0 : -100,
          stripMaxY: index === 0 ? 200 : 100,
        },
      })
      const surface = /** @type {HTMLElement} */ (view.container.querySelector('.surface'))
      const top = index * 100
      surface.getBoundingClientRect = () => /** @type {DOMRect} */ ({
        x: 0, y: top, left: 0, top, right: 100, bottom: top + 100, width: 100, height: 100, toJSON() {},
      })
      surface.setPointerCapture = vi.fn()
      surface.releasePointerCapture = vi.fn()
      return surface
    })
    return { first, second, surfaces }
  }

  it('keeps a clicked polygon one draft on its first page when a click lands on the next', async () => {
    const { first, surfaces } = mountPair('shapes', {
      shapes: { shape: 'polygon', mode: 'solid', feather: 0, color: '#ffffff', opacity: 100 },
    })
    await fireEvent.pointerDown(surfaces[0], { pointerId: 1, button: 0, clientX: 20, clientY: 80 })
    await fireEvent.pointerDown(surfaces[1], { pointerId: 1, button: 0, clientX: 60, clientY: 130 })
    await fireEvent.pointerDown(surfaces[1], { pointerId: 1, button: 0, clientX: 20, clientY: 150 })
    expect(draft.active?.pageId).toBe(first.id)
    // Read against the first sheet: 130px is 130% of the first page, across the join.
    expect(draft.active?.points.map((point) => [point.x, point.y])).toEqual([[20, 80], [60, 130], [20, 150]])
  })

  it('closes a polygon by a double-click on the next page and commits it from its first page', async () => {
    const { surfaces } = mountPair('shapes', {
      shapes: { shape: 'polygon', mode: 'solid', feather: 0, color: '#ffffff', opacity: 100 },
    })
    const createRegion = vi.fn(async () => null)
    setBackend(/** @type {any} */ ({ createRegion }))
    await fireEvent.pointerDown(surfaces[0], { pointerId: 1, button: 0, clientX: 20, clientY: 80 })
    await fireEvent.pointerDown(surfaces[1], { pointerId: 1, button: 0, clientX: 60, clientY: 130 })
    await fireEvent.pointerDown(surfaces[1], { pointerId: 1, button: 0, clientX: 20, clientY: 150 })
    await fireEvent.dblClick(surfaces[1], { clientX: 20, clientY: 150 })
    await waitFor(() => expect(createRegion).toHaveBeenCalledTimes(1))
    const request = createRegion.mock.calls[0][0]
    expect(request.pageIndex).toBe(0)
    expect(request.params.painted.kind).toBe('polygon')
    expect(request.params.painted.points.map((point) => [point.x, point.y])).toEqual([[20, 80], [60, 130], [20, 150]])
    expect(request.bbox.y + request.bbox.h).toBeCloseTo(150)
  })

  it('selects a region of the page above by its part that hangs across the join', async () => {
    const hanging = { id: 'c1-p001-d1', pageId: 'c1-p001', bbox: { x: 10, y: 90, w: 30, h: 30 }, source: 'detect', outcome: 'detected' }
    const { surfaces } = mountPair('shapes', {
      shapes: { shape: 'rect', mode: 'solid', feather: 0, color: '#ffffff', opacity: 100 },
    }, [hanging])
    // 110px down is 10% into the second page, and 110% of the first.
    await fireEvent.pointerDown(surfaces[1], { pointerId: 2, button: 0, clientX: 20, clientY: 110 })
    await fireEvent.pointerUp(surfaces[1], { pointerId: 2, button: 0, clientX: 20, clientY: 110 })
    expect(editor.selectionId).toBe(hanging.id)
  })
})

describe('the selection tool on a mounted surface', () => {
  /**
   * @param {'brush'|'lasso'|'rect'} shape
   * @param {any[]} [regions]
   */
  function mount(shape, regions = []) {
    const only = { ...page('c1-p001', 0), regions }
    editor.chapter = { id: 'c1', review: [], pages: [only] }
    editor.project = /** @type {any} */ ({ mode: 'single' })
    editor.history = createHistory()
    editor.pageIndex = 0
    editor.tool = 'maskSelect'
    editor.toolParams = { maskSelect: { mode: 'add', shape, size: 160 } }
    resetDraftState()
    const editDetectionMask = vi.fn(async () => ({ pageStatus: 'detected', changed: [], created: [], removed: [] }))
    setBackend(/** @type {any} */ ({ editDetectionMask }))
    const view = render(DrawLayer, { props: { page: only } })
    const surface = view.getByRole('button')
    surface.getBoundingClientRect = () => /** @type {DOMRect} */ ({
      x: 0, y: 0, left: 0, top: 0, right: 100, bottom: 100, width: 100, height: 100, toJSON() {},
    })
    surface.setPointerCapture = vi.fn()
    surface.releasePointerCapture = vi.fn()
    return { view, surface, editDetectionMask }
  }

  it('keeps every vertex of a lasso, whatever the brush size, and draws no footprint', async () => {
    const { view, surface, editDetectionMask } = mount('lasso')
    await fireEvent.pointerMove(surface, { clientX: 50, clientY: 50 })
    expect(view.container.querySelector('svg.cursor')).toBeNull()

    await fireEvent.pointerDown(surface, { pointerId: 1, button: 0, clientX: 20, clientY: 20 })
    for (let step = 1; step <= 20; step += 1) {
      await fireEvent.pointerMove(surface, { pointerId: 1, buttons: 1, clientX: 20 + step, clientY: 20 + step / 2 })
    }
    for (let step = 1; step <= 20; step += 1) {
      await fireEvent.pointerMove(surface, { pointerId: 1, buttons: 1, clientX: 40 - step, clientY: 30 + step })
    }
    expect(view.container.querySelector('svg.cursor')).toBeNull()
    await fireEvent.pointerUp(surface, { pointerId: 1, button: 0, clientX: 20, clientY: 50 })

    await waitFor(() => expect(editDetectionMask).toHaveBeenCalledTimes(1))
    const { painted } = editDetectionMask.mock.calls[0][0]
    expect(painted.kind).toBe('polygon')
    // A 160 px brush used to space them 2.4% of the page apart: every 1%
    // move here is its own vertex.
    expect(painted.points).toHaveLength(41)
  })

  it('previews an add in the colour of the detection it will join', async () => {
    const saved = { inside: session.maskColor, outside: session.outsideMaskColor }
    setMaskColor('#0284c7')
    setOutsideMaskColor('#c2410c')
    try {
      const detection = (id, x, insideBubble) => ({
        id, pageId: 'c1-p001', outcome: 'detected', source: 'auto', insideBubble,
        bbox: { x, y: 10, w: 20, h: 20 }, mask: null,
      })
      const { view, surface } = mount('rect', [detection('d1', 10, true), detection('d2', 50, false)])
      const preview = () => /** @type {SVGElement} */ (view.container.querySelector('svg.preview'))
      // Mostly over the outside detection.
      await fireEvent.pointerDown(surface, { pointerId: 1, button: 0, clientX: 52, clientY: 12 })
      await fireEvent.pointerMove(surface, { pointerId: 1, buttons: 1, clientX: 60, clientY: 20 })
      expect(preview().style.getPropertyValue('--mask-color')).toBe('#c2410c')
      // jsdom writes the colour back as `rgb()`.
      expect(view.container.querySelector('svg.preview rect.shape')?.getAttribute('style')).toContain('rgb(194, 65, 12)')
      // Dragged back over the speech bubble one.
      await fireEvent.pointerMove(surface, { pointerId: 1, buttons: 1, clientX: 20, clientY: 20 })
      expect(preview().style.getPropertyValue('--mask-color')).toBe('#0284c7')
    } finally {
      setMaskColor(saved.inside)
      setOutsideMaskColor(saved.outside)
    }
  })

  it('detects in the rectangle that is drawn while the mode is Detect', async () => {
    const { view, surface, editDetectionMask } = mount('lasso')
    // The shape row is away in this mode, and the lasso it still holds is not drawn.
    editor.toolParams = { maskSelect: { mode: 'detect', shape: 'lasso', size: 160 } }
    const runClean = vi.fn(async () => ({ runId: null, pages: [] }))
    setBackend(/** @type {any} */ ({ runClean, editDetectionMask }))
    await fireEvent.pointerMove(surface, { clientX: 50, clientY: 50 })
    expect(view.container.querySelector('svg.cursor')).toBeNull()

    await fireEvent.pointerDown(surface, { pointerId: 1, button: 0, clientX: 20, clientY: 30 })
    await fireEvent.pointerMove(surface, { pointerId: 1, buttons: 1, clientX: 45, clientY: 40 })
    await fireEvent.pointerMove(surface, { pointerId: 1, buttons: 1, clientX: 60, clientY: 70 })
    // The plain mark of an area a model will look at, not a mask being added.
    const drawn = view.container.querySelector('svg.preview rect.shape')
    expect(drawn?.getAttribute('width')).toBe('40')
    expect(drawn?.getAttribute('style')).toBeNull()
    await fireEvent.pointerUp(surface, { pointerId: 1, button: 0, clientX: 60, clientY: 70 })

    await waitFor(() => expect(runClean).toHaveBeenCalledTimes(1))
    const spec = runClean.mock.calls[0][0]
    expect(spec).toMatchObject({ scope: 'page', mode: 'detect', chapterId: 'c1', pageIndex: 0 })
    expect(spec.area).toEqual({ x: 20, y: 30, w: 40, h: 40 })
    // The user pointed at the text: nothing there is held back for review.
    expect(spec).toMatchObject({ textPolicy: 'all_text', outsideBubbles: 'clean' })
    // The drag asked the detector; it is not a mask of its own.
    expect(editDetectionMask).not.toHaveBeenCalled()
    expect(view.container.querySelector('svg.preview rect.shape')).toBeNull()
  })

  it('keeps the region menu over a region, and offers nothing over bare paper', async () => {
    const detection = {
      id: 'd1', pageId: 'c1-p001', outcome: 'detected', source: 'auto', insideBubble: true,
      bbox: { x: 10, y: 10, w: 20, h: 20 }, mask: null,
    }
    const { surface } = mount('rect', [detection])
    await fireEvent.contextMenu(surface, { clientX: 20, clientY: 20 })
    expect(editor.selectionId).toBe('d1')

    // An area for the detector is drawn in the Detect mode, not asked for here.
    cleanup()
    const bare = mount('rect')
    await fireEvent.contextMenu(bare.surface, { clientX: 50, clientY: 50 })
    expect(bare.view.queryByRole('menu')).toBeNull()
  })

  it('draws the brush footprint at the brush size on the page', async () => {
    const { view, surface } = mount('brush')
    await fireEvent.pointerMove(surface, { clientX: 50, clientY: 50 })
    const ring = view.container.querySelector('svg.cursor ellipse')
    // 160 page pixels across on an 800 by 1000 page, in page percent.
    expect(Number(ring?.getAttribute('rx'))).toBeCloseTo(10)
    expect(Number(ring?.getAttribute('ry'))).toBeCloseTo(8)
  })
})
