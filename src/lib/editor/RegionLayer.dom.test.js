/**
 * Moving and turning a layer on the page, mounted: which layers get a frame
 * and a handle at all, and what one drag, one turn, one `Escape` and one
 * burst of arrow keys send to the backend and leave in history.
 *
 * The backend is a stub that answers `setLayerStyle` the way the native
 * command does - the region back, its layer changed - so every assertion is
 * about what the canvas asked for. The capability rules themselves are
 * pinned natively (`cleaner_core::patch`, `library.rs`) and in
 * `model/layers.test.js`.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { setBackend } from '../api/backend.js'
import { createHistory } from '../model/history.js'
import { displayBbox, sourceBboxOf } from '../model/layers.js'
import { editor } from '../state/editor.svelte.js'
import { clearNotices } from '../state/app.svelte.js'
import { t } from '../i18n/index.js'
import RegionLayer from './RegionLayer.svelte'

const MOVABLE = { transform: 'movable', lock: true, opacity: true }
const FIXED = { transform: 'fixed', lock: false, opacity: true }

/**
 * A cleaned layer 320×240 px at (160, 240) on a 1600×2400 page.
 *
 * @param {string} id
 * @param {{engine?: string, capabilities?: object, layer?: object, outcome?: string}} [spec]
 */
function aLayer(id, { engine = 'paint', capabilities = MOVABLE, layer = {}, outcome = 'cleaned' } = {}) {
  const bbox = { x: 10, y: 10, w: 20, h: 10 }
  return {
    id,
    pageId: 'c1-p001',
    bbox,
    source: 'hand',
    outcome,
    gateSkipCause: null,
    declineReason: null,
    unusuallyLarge: false,
    mask: {
      id: `${id}-m1`,
      regionId: id,
      sequence: 1,
      fillMode: 'solid',
      elapsedMs: 0,
      fittingReconstructed: false,
      cloudOutcome: null,
      provenance: { engine, params_snapshot: {} },
      layer: { opacity: 100, offsetX: 0, offsetY: 0, rotation: 0, locked: false, ...layer },
      capabilities,
      sourceBbox: bbox,
      appearance: `${id}-a0`,
    },
  }
}

/** @type {ReturnType<typeof vi.fn>} */
let setLayerStyle

/** The undo journal the stub backend keeps, emptied for each page mounted. */
/** @type {{seq: number, label: string}[]} */
const journal = []

beforeEach(() => {
  // Arrowing between regions scrolls the next one into view; jsdom has no
  // layout to scroll.
  Element.prototype.scrollIntoView ??= () => {}
  let answers = 0
  setLayerStyle = vi.fn(async ({ regionId, layer }) => {
    const page = editor.chapter.pages[0]
    const region = page.regions.find((candidate) => candidate.id === regionId)
    const copy = JSON.parse(JSON.stringify(region))
    // As the native answer does: the box it was made in is kept, and the
    // region's box is where it is drawn now.
    copy.mask.sourceBbox = sourceBboxOf(region)
    copy.mask.layer = layer
    copy.bbox = displayBbox(copy.mask.sourceBbox, layer, page)
    copy.mask.appearance = `${regionId}-a${++answers}`
    return copy
  })
  journal.length = 0
  setBackend(/** @type {any} */ ({
    setLayerStyle,
    historyPush: vi.fn(async ({ entry }) => {
      journal.push({ seq: journal.length + 1, label: entry.label })
      return { cursor: journal.length, entries: [...journal] }
    }),
    loadPages: vi.fn(async () => []),
  }))
})

afterEach(async () => {
  // A gesture's undo entry is written after its layer write answers: let it
  // reach this test's journal rather than the next one's.
  await new Promise((resolve) => setTimeout(resolve, 0))
  await editor.history?.running
  cleanup()
  setBackend(null)
  editor.chapter = null
  editor.selectionId = null
  editor.hoverId = null
  clearNotices()
  vi.restoreAllMocks()
})

/**
 * The page open with `regions`, the first one selected, drawn over a sheet
 * `sheetWidth` px wide - which is the zoom.
 *
 * @param {any[]} regions
 * @param {{sheetWidth?: number, props?: object}} [options]
 */
function mount(regions, { sheetWidth = 800, props = {} } = {}) {
  editor.chapter = /** @type {any} */ ({
    id: 'c1',
    review: [],
    pages: [{
      id: 'c1-p001',
      chapterId: 'c1',
      index: 0,
      width: 1600,
      height: 2400,
      status: 'cleaned',
      resident: true,
      regionCount: regions.length,
      regions,
    }],
  })
  editor.history = createHistory()
  journal.length = 0
  editor.selectionId = regions[0]?.id ?? null
  const view = render(RegionLayer, { props: { page: editor.chapter.pages[0], ...props } })
  sheetAt(view, { width: sheetWidth })
  return view
}

/**
 * Where the sheet is drawn now: `top` moves as the column scrolls, `width` is
 * the zoom.
 *
 * @param {ReturnType<typeof render>} view
 * @param {{width?: number, left?: number, top?: number}} box
 */
function sheetAt(view, { width = 800, left = 0, top = 0 }) {
  const sheet = /** @type {HTMLElement} */ (view.container.querySelector('.regions'))
  sheet.getBoundingClientRect = () => /** @type {DOMRect} */ ({
    x: left, y: top, left, top, width, height: width * 1.5,
    right: left + width, bottom: top + width * 1.5, toJSON() {},
  })
}

/** @param {ReturnType<typeof render>} view @param {string} id */
const button = (view, id) => /** @type {HTMLElement} */ (view.container.querySelector(`[data-region="${id}"]`))

/**
 * @param {HTMLElement} target
 * @param {Array<[number, number]>} path - client points; the first is the press
 */
async function drag(target, path, { release = true } = {}) {
  const [[x, y], ...rest] = path
  await fireEvent.pointerDown(target, { pointerId: 1, button: 0, clientX: x, clientY: y })
  for (const [px, py] of rest) {
    await fireEvent.pointerMove(target, { pointerId: 1, buttons: 1, clientX: px, clientY: py })
  }
  const [ex, ey] = path[path.length - 1]
  if (release) await fireEvent.pointerUp(target, { pointerId: 1, button: 0, clientX: ex, clientY: ey })
}

describe('which layers can be moved on the page', () => {
  it('gives a selected movable layer a frame and a named turn handle', () => {
    const view = mount([aLayer('c1-p001-h1')])
    const frame = view.container.querySelector('[data-layer-frame="c1-p001-h1"]')
    expect(frame).not.toBeNull()
    const handle = view.getByRole('slider', { name: 'Turn layer' })
    expect(handle.getAttribute('aria-valuenow')).toBe('0')
    expect(handle.getAttribute('tabindex')).toBe('0')
    expect(button(view, 'c1-p001-h1').classList.contains('movable')).toBe(true)
  })

  it('gives a redraw no frame, no handle and no drag', async () => {
    for (const engine of ['lama', 'flux', 'cloud']) {
      const view = mount([aLayer('c1-p001-r1', { engine, capabilities: FIXED })])
      expect(view.container.querySelector('[data-layer-frame]')).toBeNull()
      expect(view.container.querySelector('[data-turn-handle]')).toBeNull()
      const target = button(view, 'c1-p001-r1')
      expect(target.classList.contains('movable')).toBe(false)
      await drag(target, [[100, 100], [200, 180]])
      await fireEvent.keyDown(target, { key: 'ArrowRight', altKey: true })
      await fireEvent.blur(target)
      expect(setLayerStyle).not.toHaveBeenCalled()
      cleanup()
    }
  })

  it('gives a detection nothing either', () => {
    const view = mount([aLayer('c1-p001-d1', { outcome: 'detected', capabilities: { transform: 'none', lock: false, opacity: false } })])
    expect(view.container.querySelector('[data-layer-frame]')).toBeNull()
  })

  it('keeps a locked layer framed, without its handle', () => {
    const view = mount([aLayer('c1-p001-h1', { layer: { locked: true } })])
    expect(view.container.querySelector('[data-layer-frame]')?.classList.contains('locked')).toBe(true)
    expect(view.container.querySelector('[data-turn-handle]')).toBeNull()
  })
})

describe('a drag', () => {
  it('moves the same page pixels at every zoom, as one write and one undo entry', async () => {
    // Half, full and double size: the same page move takes half, the same
    // and twice the client pixels.
    for (const [sheetWidth, scale] of [[800, 0.5], [1600, 1], [3200, 2]]) {
      const view = mount([aLayer('c1-p001-h1')], { sheetWidth })
      await drag(button(view, 'c1-p001-h1'), [
        [300, 300], [300 + 20 * scale, 300], [300 + 60 * scale, 300 + 30 * scale], [300 + 100 * scale, 300 + 50 * scale],
      ])
      await waitFor(() => expect(editor.history.entries).toHaveLength(1))
      expect(setLayerStyle).toHaveBeenCalledTimes(1)
      expect(setLayerStyle.mock.calls[0][0]).toMatchObject({
        regionId: 'c1-p001-h1',
        layer: { offsetX: 100, offsetY: 50, rotation: 0, opacity: 100 },
      })
      expect(editor.history.entries[0].label).toBe('masks.command.layerMove')
      expect(editor.chapter.pages[0].regions[0].mask.layer.offsetX).toBe(100)
      cleanup()
      setLayerStyle.mockClear()
    }
  })

  it('draws the preview while it moves and writes nothing until release', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    await drag(target, [[300, 300], [340, 300]], { release: false })
    // 40 client px over an 800 px sheet of a 1600 px page: 80 px, 5%.
    expect(target.style.left).toBe('15%')
    const frame = /** @type {HTMLElement} */ (view.container.querySelector('[data-layer-frame]'))
    expect(frame.style.left).toBe('15%')
    expect(setLayerStyle).not.toHaveBeenCalled()
  })

  it('stops the layer at the page edge', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    await drag(button(view, 'c1-p001-h1'), [[300, 300], [305, 300], [9000, -9000]])
    await waitFor(() => expect(setLayerStyle).toHaveBeenCalledTimes(1))
    // The centre (320, 360) stops on the right and top edges.
    expect(setLayerStyle.mock.calls[0][0].layer).toMatchObject({ offsetX: 1280, offsetY: -360 })
  })

  it('is abandoned by Escape: the preview goes back and nothing is written', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    await drag(target, [[300, 300], [400, 350]], { release: false })
    expect(target.style.left).not.toBe('10%')
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(target.style.left).toBe('10%')
    await fireEvent.pointerUp(target, { pointerId: 1, button: 0, clientX: 400, clientY: 350 })
    expect(setLayerStyle).not.toHaveBeenCalled()
    expect(editor.history.entries).toHaveLength(0)
    // The editor's own Escape did not also clear the selection it acted on.
    expect(editor.selectionId).toBe('c1-p001-h1')
  })

  it('is abandoned when the platform takes the pointer away', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    await drag(target, [[300, 300], [400, 350]], { release: false })
    await fireEvent.lostPointerCapture(target, { pointerId: 1 })
    expect(target.style.left).toBe('10%')
    await fireEvent.pointerUp(target, { pointerId: 1, button: 0, clientX: 400, clientY: 350 })
    expect(setLayerStyle).not.toHaveBeenCalled()
  })

  it('is a click, not a move, under the slop', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    await drag(button(view, 'c1-p001-h1'), [[300, 300], [302, 301]])
    expect(setLayerStyle).not.toHaveBeenCalled()
  })
})

describe('the turn handle', () => {
  // The layer's box is 10-30% across and 10-20% down a 1600 x 2400 page, so
  // its centre - the pivot - is page pixel (320, 360): client (160, 180) on
  // the 800 px sheet every test here draws.

  it('turns the layer about its centre, as one write and one undo entry', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const handle = view.getByRole('slider', { name: 'Turn layer' })
    // From straight above the pivot round to straight right of it.
    await drag(handle, [[160, 115], [206, 134], [225, 180]])
    await waitFor(() => expect(editor.history.entries).toHaveLength(1))
    expect(setLayerStyle).toHaveBeenCalledTimes(1)
    expect(setLayerStyle.mock.calls[0][0].layer).toMatchObject({ rotation: 90, offsetX: 0, offsetY: 0 })
    expect(editor.history.entries[0].label).toBe('masks.command.layerRotate')
  })

  it('snaps with Shift and is abandoned by Escape', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const handle = view.getByRole('slider', { name: 'Turn layer' })
    await fireEvent.pointerDown(handle, { pointerId: 2, button: 0, clientX: 160, clientY: 115 })
    await fireEvent.pointerMove(handle, { pointerId: 2, clientX: 180, clientY: 117, shiftKey: true })
    expect(handle.getAttribute('aria-valuenow')).toBe('15')
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(handle.getAttribute('aria-valuenow')).toBe('0')
    await fireEvent.pointerUp(handle, { pointerId: 2, button: 0, clientX: 180, clientY: 117 })
    expect(setLayerStyle).not.toHaveBeenCalled()
  })
})

describe('a gesture that outlives the sheet it began on', () => {
  it('keeps a layer under the pointer when the column scrolls mid-drag', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    await drag(target, [[300, 300], [340, 300]], { release: false })
    // The column scrolls 200 client px under a pointer that stays still: the
    // pointer is now over a page point 400 page px further down.
    sheetAt(view, { top: -200 })
    await fireEvent.scroll(window)
    expect(Number.parseFloat(target.style.top)).toBeCloseTo(10 + (400 / 2400) * 100)
    await fireEvent.pointerUp(target, { pointerId: 1, button: 0, clientX: 340, clientY: 300 })
    await waitFor(() => expect(setLayerStyle).toHaveBeenCalledTimes(1))
    expect(setLayerStyle.mock.calls[0][0].layer).toMatchObject({ offsetX: 80, offsetY: 400 })
  })

  it('keeps a layer under the pointer when the sheet zooms mid-drag', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    // Pressed on page pixel (600, 600) at half size.
    await fireEvent.pointerDown(target, { pointerId: 1, button: 0, clientX: 300, clientY: 300 })
    sheetAt(view, { width: 1600 })
    // At full size, page pixel (1300, 1200) is client (1300, 1200).
    await fireEvent.pointerMove(target, { pointerId: 1, clientX: 1300, clientY: 1200 })
    await fireEvent.pointerUp(target, { pointerId: 1, button: 0, clientX: 1300, clientY: 1200 })
    await waitFor(() => expect(setLayerStyle).toHaveBeenCalledTimes(1))
    expect(setLayerStyle.mock.calls[0][0].layer).toMatchObject({ offsetX: 700, offsetY: 600 })
  })

  it('turns about the pivot as the page now draws it, through a scroll', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const handle = view.getByRole('slider', { name: 'Turn layer' })
    await fireEvent.pointerDown(handle, { pointerId: 2, button: 0, clientX: 160, clientY: 115 })
    // Scrolled 100 px: the pivot is now at client (160, 80).
    sheetAt(view, { top: -100 })
    await fireEvent.pointerMove(handle, { pointerId: 2, clientX: 225, clientY: 80 })
    await fireEvent.pointerUp(handle, { pointerId: 2, button: 0, clientX: 225, clientY: 80 })
    await waitFor(() => expect(setLayerStyle).toHaveBeenCalledTimes(1))
    expect(setLayerStyle.mock.calls[0][0].layer.rotation).toBe(90)
  })
})

describe('a keyboard burst that has not been written yet', () => {
  it('is where a drag starts from: no jump back, and both land in order', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    await fireEvent.keyDown(target, { key: 'ArrowRight', altKey: true, shiftKey: true })
    await fireEvent.keyDown(target, { key: 'ArrowRight', altKey: true, shiftKey: true })
    // 20 page px on a 1600 px page: 1.25%.
    expect(target.style.left).toBe('11.25%')
    await fireEvent.pointerDown(target, { pointerId: 1, button: 0, clientX: 300, clientY: 300 })
    expect(target.style.left).toBe('11.25%')
    await fireEvent.pointerMove(target, { pointerId: 1, clientX: 350, clientY: 300 })
    expect(target.style.left).toBe('17.5%')
    await fireEvent.pointerUp(target, { pointerId: 1, button: 0, clientX: 350, clientY: 300 })
    await waitFor(() => expect(editor.history.entries).toHaveLength(2))
    expect(setLayerStyle.mock.calls.map(([call]) => call.layer.offsetX)).toEqual([20, 120])
    expect(editor.chapter.pages[0].regions[0].mask.layer.offsetX).toBe(120)
  })

  it('is where a turn pivots: the frame where the burst left it', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    await fireEvent.keyDown(target, { key: 'ArrowRight', altKey: true, shiftKey: true })
    await fireEvent.keyDown(target, { key: 'ArrowRight', altKey: true, shiftKey: true })
    const handle = view.getByRole('slider', { name: 'Turn layer' })
    // 20 page px right of (320, 360) is client (170, 180) on this sheet.
    await drag(handle, [[170, 115], [216, 134], [235, 180]])
    await waitFor(() => expect(editor.history.entries).toHaveLength(2))
    expect(setLayerStyle.mock.calls.map(([call]) => [call.layer.offsetX, call.layer.rotation])).toEqual([[20, 0], [20, 90]])
  })

  it('is written when the page leaves the editor', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    for (let n = 0; n < 3; n += 1) await fireEvent.keyDown(target, { key: 'ArrowRight', altKey: true })
    expect(setLayerStyle).not.toHaveBeenCalled()
    view.unmount()
    await waitFor(() => expect(setLayerStyle).toHaveBeenCalledTimes(1))
    expect(setLayerStyle.mock.calls[0][0].layer).toMatchObject({ offsetX: 3 })
    await waitFor(() => expect(editor.history.entries).toHaveLength(1))
  })
})

describe('the keyboard', () => {
  it('nudges with Alt and the arrows, and writes the burst once', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    target.focus()
    await fireEvent.keyDown(target, { key: 'ArrowRight', altKey: true })
    await fireEvent.keyDown(target, { key: 'ArrowRight', altKey: true })
    await fireEvent.keyDown(target, { key: 'ArrowDown', altKey: true, shiftKey: true })
    expect(setLayerStyle).not.toHaveBeenCalled()
    await waitFor(() => expect(setLayerStyle).toHaveBeenCalledTimes(1))
    expect(setLayerStyle.mock.calls[0][0].layer).toMatchObject({ offsetX: 2, offsetY: 10 })
    await waitFor(() => expect(editor.history.entries).toHaveLength(1))
  })

  it('turns with Alt and the brackets, by key code', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const target = button(view, 'c1-p001-h1')
    // Option turns ] into a quote on a Mac; the code is still the bracket.
    await fireEvent.keyDown(target, { key: '‘', code: 'BracketRight', altKey: true })
    await fireEvent.keyDown(target, { key: '“', code: 'BracketLeft', altKey: true, shiftKey: true })
    await fireEvent.blur(target)
    await waitFor(() => expect(setLayerStyle).toHaveBeenCalledTimes(1))
    expect(setLayerStyle.mock.calls[0][0].layer).toMatchObject({ rotation: -14 })
  })

  it('turns the handle like a slider, and Escape takes the burst back', async () => {
    const view = mount([aLayer('c1-p001-h1')])
    const handle = view.getByRole('slider', { name: 'Turn layer' })
    await fireEvent.keyDown(handle, { key: 'ArrowRight' })
    await fireEvent.keyDown(handle, { key: 'PageUp' })
    expect(handle.getAttribute('aria-valuenow')).toBe('16')
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(handle.getAttribute('aria-valuenow')).toBe('0')
    await fireEvent.blur(handle)
    expect(setLayerStyle).not.toHaveBeenCalled()

    await fireEvent.keyDown(handle, { key: 'ArrowLeft' })
    await fireEvent.blur(handle)
    await waitFor(() => expect(setLayerStyle).toHaveBeenCalledTimes(1))
    expect(setLayerStyle.mock.calls[0][0].layer).toMatchObject({ rotation: -1 })
  })

  it('sends the slider to its ends with Home and End, and 0 stands it upright', async () => {
    const view = mount([aLayer('c1-p001-h1', { layer: { rotation: 30 } })])
    const handle = view.getByRole('slider', { name: 'Turn layer' })
    expect(handle.getAttribute('aria-valuemin')).toBe('-180')
    expect(handle.getAttribute('aria-valuemax')).toBe('180')
    await fireEvent.keyDown(handle, { key: 'Home' })
    expect(handle.getAttribute('aria-valuenow')).toBe('-180')
    await fireEvent.keyDown(handle, { key: 'End' })
    expect(handle.getAttribute('aria-valuenow')).toBe('180')
    await fireEvent.keyDown(handle, { key: '0' })
    expect(handle.getAttribute('aria-valuenow')).toBe('0')
    await fireEvent.blur(handle)
    await waitFor(() => expect(setLayerStyle).toHaveBeenCalledTimes(1))
    expect(setLayerStyle.mock.calls[0][0].layer).toMatchObject({ rotation: 0 })
  })

  it('leaves the plain arrows to move between regions', async () => {
    const view = mount([aLayer('c1-p001-h1'), { ...aLayer('c1-p001-h2'), bbox: { x: 60, y: 60, w: 10, h: 5 } }])
    await fireEvent.keyDown(button(view, 'c1-p001-h1'), { key: 'ArrowRight' })
    expect(editor.selectionId).toBe('c1-p001-h2')
    await fireEvent.blur(button(view, 'c1-p001-h1'))
    expect(setLayerStyle).not.toHaveBeenCalled()
  })
})

describe('regions that are not layers yet', () => {
  const NO_OUTPUT = { transform: 'none', lock: false, opacity: false }

  it('draws a held candidate always, named as held for the user, with no badge', () => {
    const candidate = {
      ...aLayer('c1-p001-c1'),
      outcome: 'candidate',
      candidateReason: 'review.reason.unassignedMask',
      mask: null,
    }
    // The layer is the one selected: the candidate is drawn without it.
    const view = mount([aLayer('c1-p001-h1'), { ...candidate, bbox: { x: 60, y: 60, w: 10, h: 5 } }])
    const target = button(view, 'c1-p001-c1')
    expect(target.classList.contains('candidate')).toBe(true)
    expect(target.classList.contains('marked')).toBe(false)
    expect(target.querySelector('.badge')).toBeNull()
    const name = `${t('review.candidate.title')} · ${t('review.state.candidate')}`
    expect(view.getByRole('button', { name })).toBe(target)
  })

  it('keeps a detection flagged for repair a detection: its class, its name and reason, and a flag mark with the overlay', async () => {
    const flagged = {
      ...aLayer('c1-p001-d1', { outcome: 'detected', capabilities: NO_OUTPUT }),
      attention: 'review.reason.repairNeeded',
    }
    const view = mount([flagged])
    const target = button(view, 'c1-p001-d1')
    expect(target.classList.contains('detected')).toBe(true)
    expect(target.classList.contains('review')).toBe(false)
    expect(target.querySelector('.badge')).toBeNull()
    expect(target.getAttribute('aria-label')).toBe(
      `${t('masks.title.detected')} · ${t('masks.status.detected')}. ${t('review.reason.repairNeeded')}.`,
    )
    editor.maskOverlay = true
    try {
      await waitFor(() => expect(target.querySelector('.badge')?.textContent).toBe('△'))
      expect(target.classList.contains('detected')).toBe(true)
    } finally {
      editor.maskOverlay = false
    }
  })
})

/**
 * **A detection draws no box.** After a Detect the page shows each detection
 * as its mask (`DetectionMasks.svelte`), and a box over the mask would say
 * where the detector looked on top of what Clean will erase. The button stays
 * for everything else a region is: hover, selection, the menu, the keyboard.
 *
 * Component styles are not loaded under jsdom, so the half that is CSS is
 * read from the component's own stylesheet: every rule that reaches a
 * detection draws nothing, except the keyboard's focus ring.
 */
describe('a detection on the page', () => {
  const NO_OUTPUT = { transform: 'none', lock: false, opacity: false }

  it('keeps its button: hover lights it and a click selects it', async () => {
    const view = mount([aLayer('c1-p001-h1'), aLayer('c1-p001-d1', { outcome: 'detected', capabilities: NO_OUTPUT })])
    const target = button(view, 'c1-p001-d1')
    expect(target.tagName).toBe('BUTTON')
    expect(target.classList.contains('detected')).toBe(true)
    await fireEvent.pointerEnter(target)
    expect(editor.hoverId).toBe('c1-p001-d1')
    expect(target.classList.contains('lit')).toBe(true)
    await fireEvent.click(target)
    expect(editor.selectionId).toBe('c1-p001-d1')
    // Nothing drawn on the element itself either.
    expect(target.style.outline).toBe('')
    expect(target.style.background).toBe('')
  })

  it('draws no outline and no wash, lit or not, and keeps the focus ring', () => {
    // From the project root: under jsdom `import.meta.url` is not a file URL.
    const source = readFileSync(join(process.cwd(), 'src/lib/editor/RegionLayer.svelte'), 'utf8')
    const css = source.slice(source.indexOf('<style>'))
    const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
      .map(([, selector, body]) => ({ selector: selector.replace(/\/\*[\s\S]*?\*\//g, '').trim(), body }))
      .filter(({ selector }) => selector.includes('.region.detected'))
    expect(rules.length).toBeGreaterThan(0)
    const focus = rules.filter(({ selector }) => selector.includes(':focus-visible'))
    const drawn = rules.filter(({ selector }) => !selector.includes(':focus-visible'))
    expect(focus.some(({ body }) => /outline:\s*2px solid var\(--page-mark\)/.test(body))).toBe(true)
    expect(drawn.length).toBeGreaterThan(0)
    for (const { selector, body } of drawn) {
      expect(body, selector).toMatch(/outline-color:\s*transparent/)
      expect(body, selector).toMatch(/background:\s*transparent/)
      expect(body, selector).not.toMatch(/var\(--page-mark/)
    }
    // Lit and under the mask overlay both: the rule names them outright.
    const selectors = drawn.map(({ selector }) => selector).join(',')
    expect(selectors).toContain('.region.detected.lit')
    expect(selectors).toContain('.region.detected.outlined')
  })
})
