/**
 * The detected masks, mounted: which regions draw one, where, in what colour,
 * and how a lit one differs - over the stand-in the mock draws and over the
 * mask image the tile protocol serves inside the app.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render, waitFor } from '@testing-library/svelte'
import { flushSync } from 'svelte'

import { editor } from '../state/editor.svelte.js'
import { session, setMaskColor, setMaskOpacity, setOutsideMaskColor } from '../state/session.svelte.js'
import { forgetMaskImages } from './detectionmasks.svelte.js'
import DetectionMasks from './DetectionMasks.svelte'

/**
 * @param {string} id
 * @param {string} outcome
 * @param {{x: number, y: number, w: number, h: number}} bbox
 */
function region(id, outcome, bbox = { x: 10, y: 20, w: 30, h: 5 }) {
  return {
    id,
    pageId: 'c1-p001',
    bbox,
    source: 'auto',
    outcome,
    mask: outcome === 'candidate' ? null : { id: `${id}-m1`, provenance: { engine: 'fill', mask_sha256: `${id}-sha` } },
  }
}

/** @param {any[]} regions */
function aPage(regions) {
  return { id: 'c1-p001', chapterId: 'c1', index: 0, width: 1600, height: 2400, regions }
}

const saved = { color: session.maskColor, outside: session.outsideMaskColor, opacity: session.maskOpacity }

beforeEach(() => {
  setMaskColor('#0284c7')
  setOutsideMaskColor('#c2410c')
  setMaskOpacity(35)
})

afterEach(() => {
  cleanup()
  editor.hoverId = null
  editor.selectionId = null
  setMaskColor(saved.color)
  setOutsideMaskColor(saved.outside)
  setMaskOpacity(saved.opacity)
  delete globalThis.__TAURI_INTERNALS__
  forgetMaskImages()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/** @param {HTMLElement} container */
const drawnIds = (container) => [...container.querySelectorAll('[data-detection]')].map((node) => node.getAttribute('data-detection'))

describe('without the tile protocol (the mock)', () => {
  it('draws each detection, and nothing for a layer or a held candidate', () => {
    const view = render(DetectionMasks, {
      props: {
        page: aPage([
          region('d1', 'detected'),
          region('l1', 'cleaned'),
          region('c1', 'candidate'),
          region('d2', 'detected', { x: 50, y: 60, w: 10, h: 10 }),
        ]),
      },
    })
    expect(drawnIds(view.container)).toEqual(['d1', 'd2'])
    const layer = view.container.querySelector('[data-detection-masks]')
    // One named button per detection lives in RegionLayer already.
    expect(layer?.getAttribute('aria-hidden')).toBe('true')
  })

  it('stands the box in for the mask, in the selection colour at its opacity', () => {
    const view = render(DetectionMasks, { props: { page: aPage([region('d1', 'detected')]) } })
    const standin = /** @type {HTMLElement} */ (view.container.querySelector('[data-detection="d1"]'))
    expect(standin.dataset.mask).toBe('standin')
    expect(standin.style.left).toBe('10%')
    expect(standin.style.top).toBe('20%')
    expect(standin.style.width).toBe('30%')
    expect(standin.style.height).toBe('5%')
    expect(standin.style.backgroundColor).toBe('rgba(2, 132, 199, 0.35)')
    expect(standin.style.getPropertyValue('--mask-color')).toBe('#0284c7')
  })

  it('follows Settings when the colour or the opacity moves', () => {
    const view = render(DetectionMasks, { props: { page: aPage([region('d1', 'detected')]) } })
    const standin = /** @type {HTMLElement} */ (view.container.querySelector('[data-detection="d1"]'))
    setMaskColor('#ff0000')
    setMaskOpacity(60)
    flushSync()
    expect(standin.style.backgroundColor).toBe('rgba(255, 0, 0, 0.6)')
    expect(standin.style.getPropertyValue('--mask-color')).toBe('#ff0000')
  })

  it('draws text outside bubbles in its own colour, and follows Settings for each', () => {
    const inside = { ...region('d1', 'detected'), insideBubble: true }
    const outside = { ...region('d2', 'detected', { x: 50, y: 60, w: 10, h: 10 }), insideBubble: false }
    // A detection from before the flag: the speech bubble colour, as before the split.
    const unknown = region('d3', 'detected', { x: 70, y: 80, w: 10, h: 10 })
    const view = render(DetectionMasks, { props: { page: aPage([inside, outside, unknown]) } })
    const drawn = (/** @type {string} */ id) =>
      /** @type {HTMLElement} */ (view.container.querySelector(`[data-detection="${id}"]`))
    expect(drawn('d1').style.getPropertyValue('--mask-color')).toBe('#0284c7')
    expect(drawn('d2').style.getPropertyValue('--mask-color')).toBe('#c2410c')
    expect(drawn('d2').style.backgroundColor).toBe('rgba(194, 65, 12, 0.35)')
    expect(drawn('d3').style.getPropertyValue('--mask-color')).toBe('#0284c7')
    setOutsideMaskColor('#00ff00')
    flushSync()
    expect(drawn('d2').style.getPropertyValue('--mask-color')).toBe('#00ff00')
    expect(drawn('d1').style.getPropertyValue('--mask-color')).toBe('#0284c7')
    expect(drawn('d3').style.getPropertyValue('--mask-color')).toBe('#0284c7')
  })

  it('lights the one under the pointer or selected, and only that one', () => {
    const view = render(DetectionMasks, {
      props: { page: aPage([region('d1', 'detected'), region('d2', 'detected', { x: 50, y: 60, w: 10, h: 10 })]) },
    })
    const [first, second] = /** @type {HTMLElement[]} */ ([...view.container.querySelectorAll('[data-detection]')])
    expect(first.classList.contains('lit')).toBe(false)
    editor.hoverId = 'd1'
    flushSync()
    expect(first.classList.contains('lit')).toBe(true)
    expect(second.classList.contains('lit')).toBe(false)
    // Stronger, as well as thicker.
    expect(first.style.backgroundColor).toBe('rgba(2, 132, 199, 0.5)')
    editor.hoverId = null
    editor.selectionId = 'd2'
    flushSync()
    expect(first.classList.contains('lit')).toBe(false)
    expect(second.classList.contains('lit')).toBe(true)
  })

  it('draws nothing on a page with no detection', () => {
    const view = render(DetectionMasks, { props: { page: aPage([region('l1', 'cleaned')]) } })
    expect(view.container.querySelector('[data-detection-masks]')).toBeNull()
  })
})

describe('inside the app', () => {
  /** A 2D context that records what was drawn, since jsdom has none. */
  function stubCanvas() {
    const drawn = []
    const context = {
      globalCompositeOperation: 'source-over',
      globalAlpha: 1,
      fillStyle: '',
      clearRect: () => {},
      fillRect: () => {},
      drawImage: (...args) => drawn.push(args),
    }
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(() => /** @type {any} */ (context))
    // jsdom lays nothing out; the canvas is 120 by 40 CSS pixels here.
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(function () {
      return this instanceof HTMLCanvasElement ? 120 : 0
    })
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function () {
      return this instanceof HTMLCanvasElement ? 40 : 0
    })
    return drawn
  }

  beforeEach(() => {
    globalThis.__TAURI_INTERNALS__ = { convertFileSrc: (path, protocol) => `${protocol}://localhost/${path}` }
    vi.stubGlobal('createImageBitmap', vi.fn(async () => ({ bitmap: true })))
  })

  /** @param {string|null} bounds */
  const answer = (bounds) => ({
    ok: true,
    status: 200,
    headers: { get: (name) => (name === 'x-mask-bounds' ? bounds : null) },
    blob: async () => new Blob(['png']),
  })

  it('fetches the mask and places it from the bounds header, with room for the outline', async () => {
    const drawn = stubCanvas()
    const fetcher = vi.fn(async () => answer('160,480,320,120'))
    vi.stubGlobal('fetch', fetcher)
    const view = render(DetectionMasks, { props: { page: aPage([region('d1', 'detected')]) } })

    const canvas = /** @type {HTMLCanvasElement} */ (await waitFor(() => {
      const node = view.container.querySelector('canvas[data-detection="d1"]')
      expect(node).not.toBeNull()
      return node
    }))
    expect(fetcher).toHaveBeenCalledWith('tile://localhost/c1/0/detection/d1?v=d1-sha.1', expect.anything())
    expect(canvas.dataset.mask).toBe('image')
    expect(canvas.style.left).toBe('calc(10% - 3px)')
    expect(canvas.style.top).toBe('calc(20% - 3px)')
    expect(canvas.style.width).toBe('calc(20% + 6px)')
    expect(canvas.style.height).toBe('calc(5% + 6px)')
    // The backing store is the drawn size times the pixel ratio, and the
    // mask went into it.
    await waitFor(() => expect(drawn.length).toBeGreaterThan(0))
    const ratio = globalThis.devicePixelRatio || 1
    expect(canvas.width).toBe(Math.round(120 * ratio))
    expect(canvas.height).toBe(Math.round(40 * ratio))
  })

  it('fetches again only the mask an edit moved', async () => {
    stubCanvas()
    const fetcher = vi.fn(async () => answer('0,0,10,10'))
    vi.stubGlobal('fetch', fetcher)
    const view = render(DetectionMasks, {
      props: { page: aPage([region('d1', 'detected'), region('d2', 'detected')]) },
    })
    await waitFor(() => expect(view.container.querySelectorAll('canvas')).toHaveLength(2))
    expect(fetcher).toHaveBeenCalledTimes(2)
    // The reloaded page after an edit of d1 that left its mask file's digest
    // alone: only its sequence moved.
    const edited = region('d1', 'detected')
    edited.mask = { ...edited.mask, sequence: 2 }
    await view.rerender({ page: aPage([edited, region('d2', 'detected')]) })
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(3))
    expect(fetcher.mock.calls[2][0]).toBe('tile://localhost/c1/0/detection/d1?v=d1-sha.2')
  })

  it('stands the box in when the canvas cannot be drawn into', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(() => null)
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(function () {
      return this instanceof HTMLCanvasElement ? 120 : 0
    })
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function () {
      return this instanceof HTMLCanvasElement ? 40 : 0
    })
    vi.stubGlobal('fetch', vi.fn(async () => answer('0,0,10,10')))
    const view = render(DetectionMasks, { props: { page: aPage([region('d1', 'detected')]) } })
    await waitFor(() => expect(view.container.querySelector('[data-mask="standin"]')).not.toBeNull())
    expect(view.container.querySelector('canvas')).toBeNull()
  })

  it('stands the box in when the mask will not load', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.stubGlobal('fetch', vi.fn(async () => answer(null)))
    const view = render(DetectionMasks, { props: { page: aPage([region('d1', 'detected')]) } })
    await waitFor(() => expect(view.container.querySelector('[data-mask="standin"]')).not.toBeNull())
    expect(view.container.querySelector('canvas')).toBeNull()
  })

  it('draws nothing while the mask is on its way, and abandons the request when the page goes', async () => {
    /** @type {AbortSignal|undefined} */
    let signal
    vi.stubGlobal('fetch', vi.fn((_url, init) => {
      signal = init?.signal
      return new Promise(() => {})
    }))
    const view = render(DetectionMasks, { props: { page: aPage([region('d1', 'detected')]) } })
    await waitFor(() => expect(signal).toBeDefined())
    expect(view.container.querySelector('[data-detection]')).toBeNull()
    view.unmount()
    expect(signal?.aborted).toBe(true)
  })
})
