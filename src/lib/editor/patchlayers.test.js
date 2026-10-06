import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import {
  LAYER_BOUNDS_HEADER,
  cachedLayer,
  forgetLayerImages,
  loadLayer,
  pageLayers,
} from './patchlayers.svelte.js'

/** @param {string|null} bounds @param {{status?: number}} [init] */
function response(bounds, { status = 200 } = {}) {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: { get: (name) => (name === LAYER_BOUNDS_HEADER ? bounds : null) },
    blob: async () => ({}),
  }
}

const decode = async () => ({ width: 10, height: 20 })

/**
 * @param {string} id
 * @param {{order?: number, y?: number, h?: number, opacity?: number, outcome?: string}} [spec]
 */
function region(id, { order = 0, y = 10, h = 10, opacity, outcome = 'cleaned' } = {}) {
  return {
    id,
    outcome,
    bbox: { x: 10, y, w: 20, h },
    mask: { id: `${id}-m1`, layerKey: `k-${id}`, order, layer: opacity === undefined ? undefined : { opacity } },
  }
}

/** @param {number} index @param {object[]} regions */
function page(index, regions = []) {
  return { id: `c-p${index}`, chapterId: 'c', index, sourceSha: `s${index}`, width: 800, height: 1000, regions }
}

beforeEach(() => {
  globalThis.__TAURI_INTERNALS__ = { convertFileSrc: (_path, scheme) => `${scheme}://localhost/` }
})

afterEach(() => {
  delete globalThis.__TAURI_INTERNALS__
  forgetLayerImages()
})

describe('the layers a page draws', () => {
  it('are its patches in compositing order, lower first and ties by id, at their opacity', () => {
    const only = page(0, [region('b', { order: 2 }), region('c', { order: 1, opacity: 40 }), region('a', { order: 2 })])
    const layers = pageLayers(only, [only])
    expect(layers.map((layer) => layer.id)).toEqual(['c', 'a', 'b'])
    expect(layers.map((layer) => layer.opacity)).toEqual([0.4, 1, 1])
    expect(layers.every((layer) => layer.own)).toBe(true)
  })

  it('leave out detections and regions with no patch: they draw nothing', () => {
    const only = page(0, [region('d', { outcome: 'detected' }), { id: 'n', outcome: 'declined', bbox: {}, mask: null }])
    expect(pageLayers(only, [only])).toEqual([])
  })

  it('take a neighbour\'s patch that reaches across the join, and only that one', () => {
    const first = page(0, [region('spans', { y: 95, h: 10 }), region('stays', { y: 10 })])
    const second = page(1, [region('own', { y: 50 })])
    const layers = pageLayers(second, [first, second])
    expect(layers.map((layer) => [layer.id, layer.own])).toEqual([['own', true], ['spans', false]])
    expect(layers.find((layer) => layer.id === 'spans')?.url).toMatch(/\/c\/1\/layer\/spans\?v=/)
    expect(pageLayers(page(2), [first, second, page(2)])).toEqual([])
  })

  it('are none outside a Tauri window, where the mock draws its stand-in', () => {
    delete globalThis.__TAURI_INTERNALS__
    const only = page(0, [region('a')])
    expect(pageLayers(only, [only])).toEqual([])
  })
})

describe('loading a layer', () => {
  it('shares a request already in flight, then answers from the cache', async () => {
    const fetcher = vi.fn(async () => response('1,2,10,20'))
    const [a, b] = await Promise.all([loadLayer('u1', { fetcher, decode }), loadLayer('u1', { fetcher, decode })])
    expect(a).toBe(b)
    expect(a.bounds).toEqual({ x: 1, y: 2, w: 10, h: 20 })
    expect(cachedLayer('u1')).toBe(a)
    await loadLayer('u1', { fetcher, decode })
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('takes a 204 as nothing to draw, and keeps that answer', async () => {
    const fetcher = vi.fn(async () => response(null, { status: 204 }))
    expect(await loadLayer('none', { fetcher, decode })).toEqual({ image: null, bounds: null })
    await loadLayer('none', { fetcher, decode })
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('rejects a failed response and keeps nothing, so the next ask tries again', async () => {
    const fetcher = vi.fn(async () => response(null, { status: 404 }))
    await expect(loadLayer('gone', { fetcher, decode })).rejects.toThrow('image_fetch_failed')
    expect(cachedLayer('gone')).toBeUndefined()
    await expect(loadLayer('gone', { fetcher, decode })).rejects.toThrow()
    expect(fetcher).toHaveBeenCalledTimes(2)
  })
})
