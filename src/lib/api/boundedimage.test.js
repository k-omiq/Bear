import { afterEach, describe, expect, it, vi } from 'vitest'

import { ImageCache } from './boundedimage.js'

const entry = (w, h) => ({ image: /** @type {any} */ ({ width: w, height: h, close: vi.fn() }), bounds: { x: 0, y: 0, w, h } })

describe('an image cache', () => {
  it('drops the least recently asked for until it fits its weight', () => {
    const cache = new ImageCache(100, (item) => item.bounds.w * item.bounds.h)
    cache.set('a', entry(5, 8))
    cache.set('b', entry(5, 8))
    cache.get('a')
    cache.set('c', entry(5, 8))
    expect([...cache.entries.keys()]).toEqual(['a', 'c'])
    expect(cache.weight).toBe(80)
  })

  it('keeps an entry heavier than the whole budget rather than evicting the one just set', () => {
    const cache = new ImageCache(10, (item) => item.bounds.w * item.bounds.h)
    cache.set('a', entry(1, 1))
    cache.set('big', entry(10, 10))
    expect([...cache.entries.keys()]).toEqual(['big'])
  })

  it('replaces an entry under the same URL without counting it twice', () => {
    const cache = new ImageCache(100, (item) => item.bounds.w * item.bounds.h)
    cache.set('a', entry(5, 8))
    cache.set('a', entry(2, 2))
    expect(cache.size).toBe(1)
    expect(cache.weight).toBe(4)
  })

  describe('closing what it lets go', () => {
    afterEach(() => vi.useRealTimers())

    it('closes an evicted image a task later, when nothing holds it', () => {
      vi.useFakeTimers()
      const cache = new ImageCache(1)
      const a = entry(1, 1)
      cache.set('a', a)
      cache.set('b', entry(1, 1))
      expect(a.image.close).not.toHaveBeenCalled()
      vi.runAllTimers()
      expect(a.image.close).toHaveBeenCalledTimes(1)
    })

    it('keeps a held image open after it leaves, and closes it on the last release', () => {
      vi.useFakeTimers()
      const cache = new ImageCache(1)
      const a = entry(1, 1)
      cache.set('a', a)
      cache.hold(a)
      cache.hold(a)
      cache.set('b', entry(1, 1))
      vi.runAllTimers()
      cache.release(a)
      vi.runAllTimers()
      expect(a.image.close).not.toHaveBeenCalled()
      cache.release(a)
      vi.runAllTimers()
      expect(a.image.close).toHaveBeenCalledTimes(1)
    })

    it('never closes an image still in the cache, held or not', () => {
      vi.useFakeTimers()
      const cache = new ImageCache(10)
      const a = entry(1, 1)
      cache.set('a', a)
      cache.hold(a)
      cache.release(a)
      vi.runAllTimers()
      expect(a.image.close).not.toHaveBeenCalled()
    })

    it('closes a replaced image and everything on a clear', () => {
      vi.useFakeTimers()
      const cache = new ImageCache(10)
      const a = entry(1, 1)
      const b = entry(1, 1)
      cache.set('a', a)
      cache.set('a', b)
      cache.clear()
      vi.runAllTimers()
      expect(a.image.close).toHaveBeenCalledTimes(1)
      expect(b.image.close).toHaveBeenCalledTimes(1)
    })
  })
})
