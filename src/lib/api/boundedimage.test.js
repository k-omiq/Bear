import { describe, expect, it } from 'vitest'

import { ImageCache } from './boundedimage.js'

const entry = (w, h) => ({ image: /** @type {any} */ ({ width: w, height: h }), bounds: { x: 0, y: 0, w, h } })

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
})
