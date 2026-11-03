import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import {
  DEFAULT_NANO_BANANA_MODEL,
  NANO_BANANA_MODELS,
  nanoBananaCost,
  nanoBananaModel,
  nanoBananaStartSize,
} from './nanobanana.js'

describe('the Nano Banana models', () => {
  it('match the native list: same ids, same smallest size, same default', () => {
    const source = readFileSync(new URL('../../../crates/cleaner-core/src/engines/nano_banana.rs', import.meta.url), 'utf8')
    const native = [...source.matchAll(/pub const NANO_BANANA\w*: Model = Model \{ id: "([^"]+)", lowest: Tier::(\w+)/g)]
      .map(([, id, lowest]) => [id, { Half: '512', One: '1K', Two: '2K' }[lowest]])
    expect(NANO_BANANA_MODELS.map((model) => [model.id, model.lowest]).sort())
      .toEqual(native.sort())
    expect(source).toContain(`pub const DEFAULT: Model = NANO_BANANA_2;`)
    expect(DEFAULT_NANO_BANANA_MODEL).toBe('gemini-3.1-flash-image')
  })

  it('know a model by its id and read anything else as the default', () => {
    expect(nanoBananaModel('gemini-3-pro-image').id).toBe('gemini-3-pro-image')
    expect(nanoBananaModel('../../evil').id).toBe(DEFAULT_NANO_BANANA_MODEL)
    expect(nanoBananaModel(undefined).id).toBe(DEFAULT_NANO_BANANA_MODEL)
  })

  it('start each model at the smallest size it answers at, never a setting', () => {
    expect(nanoBananaStartSize('gemini-3.1-flash-image')).toBe('512')
    expect(nanoBananaStartSize('gemini-nano-banana-2.1')).toBe('1K')
    expect(nanoBananaStartSize('gemini-3-pro-image')).toBe('1K')
    expect(nanoBananaStartSize('../../evil')).toBe('512')
  })

  it('price from the smallest size up to the largest, since a big region moves up', () => {
    expect(nanoBananaCost(10, 'gemini-3.1-flash-image').low).toBeCloseTo(0.45)
    expect(nanoBananaCost(10, 'gemini-3.1-flash-image').high).toBeCloseTo(1.01)
    expect(nanoBananaCost(10, 'gemini-nano-banana-2.1').low).toBeCloseTo(0.336)
    expect(nanoBananaCost(10, 'gemini-3-pro-image')).toEqual({ low: 1.34, high: 1.34 })
  })
})
