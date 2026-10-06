/**
 * The detected masks' loader and painter, without a DOM: the header that says
 * where a mask sits, the cache, and the compositing that colours
 * a mask without ever reading a pixel back.
 */

import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  MASK_CACHE_LIMIT,
  MAX_CANVAS_PIXELS,
  backingScale,
  cachedMaskCount,
  forgetMaskImages,
  loadDetectionMask,
  paintMask,
  parseMaskBounds,
} from './detectionmasks.svelte.js'

/** A response the protocol would send: a PNG body and the bounds header. */
function response(bounds = '10,20,30,40', { ok = true, status = 200 } = {}) {
  return {
    ok,
    status,
    headers: { get: (name) => (name.toLowerCase() === 'x-mask-bounds' ? bounds : null) },
    blob: async () => new Blob(['png']),
  }
}

const decode = vi.fn(async () => ({ decoded: true }))

afterEach(() => {
  forgetMaskImages()
  decode.mockClear()
})

describe('the bounds header', () => {
  it('is four whole page pixels, x, y, width and height', () => {
    expect(parseMaskBounds('10,20,30,40')).toEqual({ x: 10, y: 20, w: 30, h: 40 })
    expect(parseMaskBounds(' 0, 0, 5, 6 ')).toEqual({ x: 0, y: 0, w: 5, h: 6 })
  })

  it('is refused when it is missing, short, not numbers, or empty', () => {
    for (const header of [null, undefined, '', '1,2,3', '1,2,3,4,5', 'a,b,c,d', '1,2,0,4', '1,2,3,-4']) {
      expect(parseMaskBounds(/** @type {any} */ (header)), String(header)).toBeNull()
    }
  })
})

describe('loading a mask', () => {
  it('fetches once, reads the bounds, and serves the second ask from the cache', async () => {
    const fetcher = vi.fn(async () => response())
    const first = await loadDetectionMask('tile://localhost/c/0/detection/r1?v=a', { fetcher, decode })
    const again = await loadDetectionMask('tile://localhost/c/0/detection/r1?v=a', { fetcher, decode })
    expect(first).toEqual({ image: { decoded: true }, bounds: { x: 10, y: 20, w: 30, h: 40 } })
    expect(again).toBe(first)
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('fetches again for a new version and keeps every other mask cached', async () => {
    const fetcher = vi.fn(async () => response())
    await loadDetectionMask('u?v=a.1', { fetcher, decode })
    await loadDetectionMask('w?v=c.1', { fetcher, decode })
    await loadDetectionMask('u?v=a.2', { fetcher, decode })
    expect(fetcher).toHaveBeenCalledTimes(3)
    await loadDetectionMask('w?v=c.1', { fetcher, decode })
    expect(fetcher).toHaveBeenCalledTimes(3)
  })

  it('keeps no more than its limit, dropping the least recently drawn', async () => {
    const fetcher = vi.fn(async () => response())
    for (let index = 0; index <= MASK_CACHE_LIMIT; index += 1) {
      await loadDetectionMask(`u${index}`, { fetcher, decode })
    }
    expect(cachedMaskCount()).toBe(MASK_CACHE_LIMIT)
    // The first one went; asking for it again is a fetch.
    fetcher.mockClear()
    await loadDetectionMask('u0', { fetcher, decode })
    expect(fetcher).toHaveBeenCalledTimes(1)
  })

  it('rejects a failed response or one with no bounds, and caches neither', async () => {
    await expect(loadDetectionMask('a', { fetcher: async () => response('1,2,3,4', { ok: false, status: 404 }), decode }))
      .rejects.toThrow('image_fetch_failed')
    await expect(loadDetectionMask('b', { fetcher: async () => response(null), decode })).rejects.toThrow('image_bounds_missing')
    expect(cachedMaskCount()).toBe(0)
  })

  it('passes the abort signal on, and caches nothing an abort interrupted', async () => {
    const controller = new AbortController()
    const fetcher = vi.fn(async (_url, init) => {
      expect(init.signal).toBe(controller.signal)
      controller.abort()
      return response()
    })
    await expect(loadDetectionMask('c', { fetcher, decode, signal: controller.signal })).rejects.toThrow('aborted')
    expect(cachedMaskCount()).toBe(0)
  })
})

describe('the backing store', () => {
  it('follows the display’s pixel ratio', () => {
    expect(backingScale(100, 50, 2)).toBe(2)
    expect(backingScale(100, 50, 1)).toBe(1)
  })

  it('stops short of the canvas size WebKit will refuse', () => {
    const scale = backingScale(4000, 8000, 2)
    expect(scale).toBeLessThan(1)
    expect(4000 * scale * 8000 * scale).toBeLessThanOrEqual(MAX_CANVAS_PIXELS + 1)
  })
})

/** A 2D context that records what was asked of it. */
function recorder() {
  const calls = []
  const context = {
    globalCompositeOperation: 'source-over',
    globalAlpha: 1,
    fillStyle: '',
    clearRect: (...args) => calls.push(['clearRect', ...args]),
    fillRect: (...args) => calls.push(['fillRect', context.globalCompositeOperation, context.fillStyle, ...args]),
    drawImage: (image, ...args) => calls.push(['drawImage', context.globalCompositeOperation, context.globalAlpha, image, ...args]),
    getImageData: () => { throw new Error('a mask is never read back') },
    putImageData: () => { throw new Error('a mask is never read back') },
  }
  return { calls, context }
}

describe('colouring a mask', () => {
  const originalDocument = globalThis.document

  afterEach(() => {
    globalThis.document = originalDocument
  })

  it('draws nothing, and does not throw, where there is no context', () => {
    expect(paintMask(/** @type {any} */ ({ width: 10, height: 10, getContext: () => null }), {}, {
      inset: 2, outline: 1, color: '#0284c7', fill: 0.35,
    })).toBe(false)
  })

  it('fills at the opacity and outlines at full strength, by compositing alone', () => {
    const main = recorder()
    const ring = recorder()
    const scratch = { width: 0, height: 0, getContext: () => ring.context }
    globalThis.document = /** @type {any} */ ({ createElement: () => scratch })
    const canvas = { width: 40, height: 30, getContext: () => main.context }
    const image = { mask: true }

    expect(paintMask(/** @type {any} */ (canvas), image, { inset: 3, outline: 1.5, color: '#ff0000', fill: 0.35 })).toBe(true)

    // The fill: the mask at the fill's alpha, inset for the outline's room,
    // then the colour kept only where the mask landed.
    expect(main.calls[1]).toEqual(['drawImage', 'source-over', 0.35, image, 3, 3, 34, 24])
    expect(main.calls[2]).toEqual(['fillRect', 'source-in', '#ff0000', 0, 0, 40, 30])
    // The outline: the mask grown in a ring of directions, the mask cut back
    // out, the colour kept, and the ring laid over the fill at full strength.
    const grown = ring.calls.filter(([op, mode]) => op === 'drawImage' && mode === 'source-over')
    expect(grown.length).toBeGreaterThanOrEqual(8)
    expect(ring.calls).toContainEqual(['drawImage', 'destination-out', 1, image, 3, 3, 34, 24])
    expect(ring.calls.at(-1)).toEqual(['fillRect', 'source-in', '#ff0000', 0, 0, 40, 30])
    expect(main.calls.at(-1)).toEqual(['drawImage', 'source-over', 1, scratch, 0, 0])
    expect(scratch).toMatchObject({ width: 40, height: 30 })
  })
})
