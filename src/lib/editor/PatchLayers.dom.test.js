/**
 * The cleaned side of a page, mounted inside the app: a canvas per patch over
 * the source tiles. The first test is the regression this component exists
 * for - an edit must not blank what is already drawn.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render, waitFor } from '@testing-library/svelte'

import { editor } from '../state/editor.svelte.js'
import PatchLayers from './PatchLayers.svelte'
import { LAYER_BOUNDS_HEADER, forgetLayerImages, layerDraws } from './patchlayers.svelte.js'

/**
 * @param {string} id
 * @param {{key?: string, order?: number, opacity?: number}} [spec]
 */
function region(id, { key = `k-${id}`, order = 0, opacity = 100 } = {}) {
  return {
    id,
    pageId: 'c1-p001',
    outcome: 'cleaned',
    bbox: { x: 10, y: 10, w: 20, h: 10 },
    mask: { id: `${id}-m1`, regionId: id, layerKey: key, order, layer: { opacity } },
  }
}

/** @param {any[]} regions */
function aPage(regions) {
  return { id: 'c1-p001', chapterId: 'c1', index: 0, sourceSha: 'aa', width: 800, height: 1000, resident: true, regions }
}

/** Fetches that answer only when a test says so, by URL. */
function deferredFetch() {
  /** @type {Map<string, (bounds: string) => void>} */
  const waiting = new Map()
  const fetcher = vi.fn((url) => new Promise((resolve) => {
    waiting.set(String(url), (bounds) => resolve({
      ok: true,
      status: 200,
      headers: { get: (name) => (name === LAYER_BOUNDS_HEADER ? bounds : null) },
      blob: async () => ({ bounds }),
    }))
  }))
  /** @param {string} fragment @param {string} bounds */
  const answer = async (fragment, bounds) => {
    await waitFor(() => expect([...waiting.keys()].some((url) => url.includes(fragment))).toBe(true))
    const url = /** @type {string} */ ([...waiting.keys()].find((key) => key.includes(fragment)))
    const resolve = /** @type {(bounds: string) => void} */ (waiting.get(url))
    waiting.delete(url)
    resolve(bounds)
  }
  return { fetcher, answer }
}

/** @type {Array<{image: any}>} */
let drawn = []

beforeEach(() => {
  globalThis.__TAURI_INTERNALS__ = { convertFileSrc: (_path, scheme) => `${scheme}://localhost/` }
  editor.project = /** @type {any} */ ({ mode: 'single' })
  drawn = []
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(
    /** @type {any} */ (() => ({ drawImage: (image) => drawn.push({ image }) })),
  )
  vi.stubGlobal('createImageBitmap', async (blob) => {
    const [, , w, h] = blob.bounds.split(',').map(Number)
    return { width: w, height: h, from: blob.bounds }
  })
})

afterEach(() => {
  cleanup()
  delete globalThis.__TAURI_INTERNALS__
  editor.project = null
  forgetLayerImages()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

/** @param {HTMLElement} container */
const canvases = (container) => /** @type {HTMLCanvasElement[]} */ ([...container.querySelectorAll('canvas[data-layer]')])

describe('a patch layer', () => {
  it('keeps what it drew until the new picture is decoded, then changes place and pixels together', async () => {
    const { fetcher, answer } = deferredFetch()
    vi.stubGlobal('fetch', fetcher)
    const view = render(PatchLayers, { props: { page: aPage([region('r1', { key: 'one' })]), variant: 'cleaned' } })

    await answer('/layer/r1?', '80,100,160,100')
    const [canvas] = canvases(view.container)
    await waitFor(() => expect(canvas.width).toBe(160))
    expect(canvas.style.left).toBe('10%')
    expect(canvas.style.display).toBe('block')
    expect(drawn).toHaveLength(1)

    // An edit: the layer's key moves, and the fetch is held.
    await view.rerender({ page: aPage([region('r1', { key: 'two' })]), variant: 'cleaned' })
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(2))
    expect(canvases(view.container)[0]).toBe(canvas)
    expect([canvas.width, canvas.height, canvas.style.left, canvas.style.display]).toEqual([160, 100, '10%', 'block'])
    expect(drawn).toHaveLength(1)

    await answer('/layer/r1?', '160,200,80,50')
    await waitFor(() => expect(canvas.width).toBe(80))
    expect([canvas.height, canvas.style.left, canvas.style.top]).toEqual([50, '20%', '20%'])
    expect(drawn.at(-1)?.image.from).toBe('160,200,80,50')
  })

  it('stacks the layers in compositing order, each at its opacity, and draws the moved one alone', async () => {
    const { fetcher, answer } = deferredFetch()
    vi.stubGlobal('fetch', fetcher)
    const regions = [region('b', { order: 2 }), region('a', { order: 1, opacity: 40 })]
    const view = render(PatchLayers, { props: { page: aPage(regions), variant: 'cleaned' } })
    await answer('/layer/a?', '0,0,10,10')
    await answer('/layer/b?', '0,0,10,10')
    const list = canvases(view.container)
    expect(list.map((canvas) => canvas.dataset.layer)).toEqual(['a', 'b'])
    expect(list.map((canvas) => canvas.style.opacity)).toEqual(['0.4', '1'])

    // An opacity change fetches nothing: the canvas draws it.
    const faded = [region('b', { order: 2, opacity: 70 }), region('a', { order: 1, opacity: 40 })]
    await view.rerender({ page: aPage(faded), variant: 'cleaned' })
    expect(canvases(view.container)[1].style.opacity).toBe('0.7')
    expect(fetcher).toHaveBeenCalledTimes(2)
  })

  it('counts each draw for the page, which is what a held paint stroke waits on', async () => {
    const { fetcher, answer } = deferredFetch()
    vi.stubGlobal('fetch', fetcher)
    render(PatchLayers, { props: { page: aPage([region('r1')]), variant: 'cleaned' } })
    expect(layerDraws.get('c1-p001') ?? 0).toBe(0)
    await answer('/layer/r1?', '0,0,10,10')
    await waitFor(() => expect(layerDraws.get('c1-p001')).toBe(1))
  })

  it('reads ready only once the page is in hand and every layer has drawn', async () => {
    const { fetcher, answer } = deferredFetch()
    vi.stubGlobal('fetch', fetcher)
    let ready = /** @type {string|null} */ ('stale')
    const props = {
      page: aPage([region('r1')]),
      variant: /** @type {'cleaned'} */ ('cleaned'),
      get ready() { return ready },
      set ready(value) { ready = value },
    }
    render(PatchLayers, { props })
    await waitFor(() => expect(ready).toBeNull())
    await answer('/layer/r1?', '0,0,10,10')
    await waitFor(() => expect(ready).toBe('c1-p001'))
  })
})
