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
import { editor, undo } from '../state/editor.svelte.js'
import { session, setMaskColor, setOutsideMaskColor } from '../state/session.svelte.js'
import { resetDraftState } from './draft.svelte.js'
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
  resetDraftState()
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
      expect(paint.points.map((point) => point.p)).toEqual([downExpected, moveExpected])
      expect(radiusAt(paint, paint.points[0].p)).toBe(footprintRadius * downExpected)
      expect(radiusAt(paint, paint.points[1].p)).toBe(footprintRadius * moveExpected)
      await editor.history.running
    } finally {
      cleanup()
      canvas.mockRestore()
    }
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

  it('offers Detect text here over bare paper and holds the run to that spot', async () => {
    const { view, surface } = mount('rect')
    const runClean = vi.fn(async () => ({ runId: null, pages: [] }))
    setBackend(/** @type {any} */ ({ runClean }))
    await fireEvent.contextMenu(surface, { clientX: 50, clientY: 50 })
    await fireEvent.click(view.getByRole('menuitem', { name: 'Detect text here' }))

    await waitFor(() => expect(runClean).toHaveBeenCalledTimes(1))
    const spec = runClean.mock.calls[0][0]
    expect(spec).toMatchObject({ scope: 'page', mode: 'detect', chapterId: 'c1', pageIndex: 0 })
    // 16% of an 800 px page is 128 px, which is 12.8% of its 1000 px height.
    expect(spec.area.x).toBeCloseTo(42)
    expect(spec.area.y).toBeCloseTo(43.6)
    expect(spec.area.w).toBeCloseTo(16)
    expect(spec.area.h).toBeCloseTo(12.8)
    // The user pointed at the text: nothing there is held back for review.
    expect(spec).toMatchObject({ textPolicy: 'all_text', outsideBubbles: 'clean' })
    expect(view.queryByRole('menu')).toBeNull()
  })

  it('keeps the region menu over a region, and offers nothing over bare paper for another tool', async () => {
    const detection = {
      id: 'd1', pageId: 'c1-p001', outcome: 'detected', source: 'auto', insideBubble: true,
      bbox: { x: 10, y: 10, w: 20, h: 20 }, mask: null,
    }
    const { view, surface } = mount('rect', [detection])
    await fireEvent.contextMenu(surface, { clientX: 20, clientY: 20 })
    expect(view.queryByRole('menuitem', { name: 'Detect text here' })).toBeNull()
    expect(editor.selectionId).toBe('d1')

    cleanup()
    const other = mount('rect')
    editor.tool = 'shapes'
    await fireEvent.contextMenu(other.surface, { clientX: 50, clientY: 50 })
    expect(other.view.queryByRole('menu')).toBeNull()
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
