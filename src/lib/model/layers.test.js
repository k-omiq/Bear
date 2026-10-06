/**
 * The interface's copy of the native layer rules: what a layer may do, what
 * a change it may not make is refused with, and where a moved or turned layer
 * is drawn. The native half is pinned by `cleaner_core::patch` and
 * `library.rs` tests over the same cases.
 */
import { describe, expect, it } from 'vitest'

import {
  DEFAULT_LAYER,
  NO_OUTPUT,
  canTransform,
  displayBbox,
  editedLayer,
  layerCapabilities,
  layerFrame,
  normalizeAngle,
  pagesReached,
  sanitizeLayer,
} from './layers.js'

/** @param {string} engine @param {object} [extra] */
const region = (engine, extra = {}) => ({
  outcome: 'cleaned',
  bbox: { x: 10, y: 10, w: 20, h: 10 },
  mask: { provenance: { engine }, ...extra },
})

describe('layer capabilities', () => {
  it('read the native answer when the mask carries one', () => {
    const native = { transform: 'fixed', lock: false, opacity: true }
    expect(layerCapabilities(region('fill', { capabilities: native }))).toBe(native)
  })

  it('follow the output, not the gesture, when they have to be derived', () => {
    // `denoise` is the retired rung, saved before it went: a fill now.
    for (const engine of ['fill', 'paint', 'denoise']) {
      expect(layerCapabilities(region(engine))).toEqual({ transform: 'movable', lock: true, opacity: true })
    }
    for (const engine of ['lama', 'flux', 'cloud', 'clone']) {
      expect(layerCapabilities(region(engine))).toEqual({ transform: 'fixed', lock: false, opacity: true })
    }
    // A shape stroke that ran LaMa is a LaMa redraw.
    const shapeRedraw = region('lama')
    shapeRedraw.mask.provenance.params_snapshot = { tool: 'shapes' }
    expect(layerCapabilities(shapeRedraw).transform).toBe('fixed')
  })

  it('give a detection and a maskless region no output to style', () => {
    expect(layerCapabilities({ ...region('lama'), outcome: 'detected' })).toBe(NO_OUTPUT)
    expect(layerCapabilities({ outcome: 'declined', mask: null })).toBe(NO_OUTPUT)
  })

  it('let only an unlocked movable layer start a gesture', () => {
    expect(canTransform(region('paint'))).toBe(true)
    expect(canTransform(region('paint', { layer: { locked: true } }))).toBe(false)
    expect(canTransform(region('flux'))).toBe(false)
  })
})

describe('a layer change', () => {
  const fixed = { transform: 'fixed', lock: false, opacity: true }
  const movable = { transform: 'movable', lock: true, opacity: true }

  it('refuses every move of a fixed redraw, and its lock, by catalogue key', () => {
    expect(editedLayer(DEFAULT_LAYER, { opacity: 40 }, fixed).opacity).toBe(40)
    expect(() => editedLayer(DEFAULT_LAYER, { offsetX: 3 }, fixed)).toThrow('masks.refused.fixed')
    expect(() => editedLayer(DEFAULT_LAYER, { rotation: 5 }, fixed)).toThrow('masks.refused.fixed')
    expect(() => editedLayer(DEFAULT_LAYER, { locked: true }, fixed)).toThrow('masks.refused.noLock')
  })

  it('keeps a legacy redraw move frozen: opacity edits, the move stays', () => {
    const legacy = { opacity: 100, offsetX: 20, offsetY: 12, rotation: 90, locked: false }
    expect(editedLayer(legacy, { ...legacy, opacity: 50 }, fixed)).toEqual({ ...legacy, opacity: 50 })
    expect(() => editedLayer(legacy, DEFAULT_LAYER, fixed)).toThrow('masks.refused.fixed')
  })

  it('moves a movable layer until it is locked', () => {
    expect(editedLayer(DEFAULT_LAYER, { offsetX: 12.4, rotation: 190 }, movable))
      .toEqual({ opacity: 100, offsetX: 12, offsetY: 0, rotation: -170, locked: false })
    const locked = { ...DEFAULT_LAYER, locked: true }
    expect(() => editedLayer(locked, { ...locked, offsetX: 1 }, movable)).toThrow('masks.refused.locked')
    expect(editedLayer(locked, { offsetX: 1, locked: false }, movable).offsetX).toBe(1)
  })

  it('refuses any style on a detection', () => {
    expect(() => editedLayer(DEFAULT_LAYER, { opacity: 50 }, NO_OUTPUT)).toThrow('masks.refused.noOutput')
    expect(editedLayer(DEFAULT_LAYER, {}, NO_OUTPUT)).toEqual(DEFAULT_LAYER)
  })
})

describe('angles and boxes', () => {
  it('wrap a turn past half way and keep in-range values exact', () => {
    expect(normalizeAngle(190)).toBe(-170)
    expect(normalizeAngle(-190)).toBe(170)
    expect(normalizeAngle(540)).toBe(180)
    expect(normalizeAngle(-180)).toBe(-180)
    expect(Object.is(normalizeAngle(-0), 0)).toBe(true)
    expect(sanitizeLayer({ opacity: 140, offsetX: 99999 })).toMatchObject({ opacity: 100, offsetX: 10000 })
  })

  it('draw a turned layer in the box the native side computes', () => {
    const page = { width: 200, height: 100 }
    // 40×10 px, centred at (60, 15).
    const source = { x: 20, y: 10, w: 20, h: 10 }
    expect(displayBbox(source, {}, page)).toEqual(source)
    const turned = displayBbox(source, { rotation: 90 }, page)
    // 10 wide, 40 tall, about the same centre.
    expect(turned.x).toBeCloseTo(27.5)
    expect(turned.w).toBeCloseTo(5)
    expect(turned.y).toBeCloseTo(-5)
    expect(turned.h).toBeCloseTo(40)
    expect(displayBbox(source, { offsetX: 20, offsetY: -10 }, page)).toEqual({ x: 30, y: 0, w: 20, h: 10 })
  })

  it('put the frame on the untransformed box, moved, with the turn to apply', () => {
    expect(layerFrame({ x: 20, y: 10, w: 20, h: 10 }, { offsetX: 20, offsetY: 10, rotation: 30 }, { width: 200, height: 100 }))
      .toEqual({ x: 30, y: 20, w: 20, h: 10, rotation: 30 })
  })
})

describe('the longstrip pages a layer reaches', () => {
  const heights = [1000, 1000, 1000]

  it('are the pages its boxes overlap, before and after', () => {
    expect(pagesReached(heights, 0, [{ x: 0, y: 10, w: 10, h: 10 }])).toEqual([0])
    expect(pagesReached(heights, 0, [{ x: 0, y: 95, w: 10, h: 17 }])).toEqual([0, 1])
    // Moved from the middle page onto the last: both hear of it.
    expect(pagesReached(heights, 1, [{ x: 0, y: 40, w: 10, h: 10 }, { x: 0, y: 140, w: 10, h: 10 }])).toEqual([1, 2])
  })

  it('are every page when a height is unknown', () => {
    expect(pagesReached([1000, 0, 1000], 0, [{ x: 0, y: 10, w: 10, h: 10 }])).toEqual([0, 1, 2])
  })
})
