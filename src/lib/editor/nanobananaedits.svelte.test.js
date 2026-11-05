/**
 * A single region's Nano Banana request in flight has a Stop of its own: it is
 * counted while it runs, and Stop names the grant its window made, so it ends
 * that request and nothing else.
 */
import { describe, expect, it, vi } from 'vitest'

import { nanoBananaEditOut, stopNanoBananaEdit, whileNanoBananaEdit } from './nanobananaedits.svelte.js'

describe('a single region\'s Nano Banana request', () => {
  it('is out while it runs, and Stop names its own grant', async () => {
    const backend = { nanoBananaStop: vi.fn(async () => {}) }
    /** @type {(value: string) => void} */
    let answer = () => {}
    const request = whileNanoBananaEdit('r1', 'g1', () => new Promise((resolve) => { answer = resolve }))
    expect(nanoBananaEditOut('r1')).toBe(true)
    expect(nanoBananaEditOut('r2')).toBe(false)
    expect(await stopNanoBananaEdit('r1', /** @type {any} */ (backend))).toBe(true)
    expect(backend.nanoBananaStop).toHaveBeenCalledWith({ grant: 'g1' })
    answer('stopped')
    expect(await request).toBe('stopped')
    expect(nanoBananaEditOut('r1')).toBe(false)
    // Nothing out, nothing to stop.
    expect(await stopNanoBananaEdit('r1', /** @type {any} */ (backend))).toBe(false)
    expect(backend.nanoBananaStop).toHaveBeenCalledTimes(1)
  })

  it('is not counted without a grant, and is let go of when the request fails', async () => {
    expect(await whileNanoBananaEdit('r3', null, async () => nanoBananaEditOut('r3'))).toBe(false)
    await expect(whileNanoBananaEdit('r4', 'g4', async () => { throw new Error('refused') })).rejects.toThrow('refused')
    expect(nanoBananaEditOut('r4')).toBe(false)
  })
})
