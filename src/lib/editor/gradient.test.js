/**
 * The Gradient tool's arithmetic on this side of the seam. `position` and
 * `colorAt` are `gradient.rs#Gradient::position` and `::at` written again for
 * the live preview, so the numbers here are the ones the native side's own
 * tests reach for the same lines; the rest is the bar's stops and the one
 * crossing each way.
 */

import { describe, expect, it } from 'vitest'

import {
  DEFAULT_STOPS,
  addStop,
  colorAt,
  colorBetween,
  fromWire,
  inPixels,
  opacityBetween,
  paintGradient,
  position,
  presetStops,
  snapped,
  wireGradient,
} from './gradient.js'

/** A rightward drag of 100 page pixels from (0, 0). */
const along = (kind, extra = {}) => ({ kind, start: [0, 0], end: [100, 0], ...extra })

describe('position', () => {
  it('runs 0 to 1 along a linear drag and clamps past both ends', () => {
    expect(position(along('linear'), 0, 0)).toBe(0)
    expect(position(along('linear'), 50, 30)).toBeCloseTo(0.5)
    expect(position(along('linear'), -20, 0)).toBe(0)
    expect(position(along('linear'), 140, 0)).toBe(1)
  })

  it('mirrors a reflected drag about its start', () => {
    expect(position(along('reflected'), -25, 0)).toBeCloseTo(0.25)
    expect(position(along('reflected'), 25, 0)).toBeCloseTo(0.25)
  })

  it('measures radial and diamond distance from the start', () => {
    expect(position(along('radial'), 30, 40)).toBeCloseTo(0.5)
    // |along| + |across|: 30 + 40 over a length of 100.
    expect(position(along('diamond'), 30, 40)).toBeCloseTo(0.7)
  })

  it('turns an angle gradient counter-clockwise on screen, y pointing down', () => {
    // Straight up from a rightward drag is a quarter turn.
    expect(position(along('angle'), 0, -10)).toBeCloseTo(0.25)
    expect(position(along('angle'), -10, 0)).toBeCloseTo(0.5)
    expect(position(along('angle'), 0, 10)).toBeCloseTo(0.75)
  })

  it('reverses after clamping', () => {
    expect(position(along('linear', { reverse: true }), 25, 0)).toBeCloseTo(0.75)
    expect(position(along('linear', { reverse: true }), 200, 0)).toBe(0)
  })
})

describe('colorAt', () => {
  const g = wireGradient(
    { colors: [{ at: 0, color: '#000000' }, { at: 1, color: '#ffffff' }], opacities: [{ at: 0, opacity: 100 }, { at: 1, opacity: 0 }] },
    { x: 0, y: 0 },
    { x: 100, y: 0 },
  )

  it('interpolates colour and opacity between the stops', () => {
    const { color, alpha } = colorAt(g, 0.5)
    expect(color.map(Math.round)).toEqual([128, 128, 128])
    expect(alpha).toBeCloseTo(0.5)
  })

  it('holds the end stops past them, in either order given', () => {
    const shuffled = { ...g, colors: [...g.colors].reverse() }
    expect(colorAt(shuffled, 0).color).toEqual([0, 0, 0])
    expect(colorAt(shuffled, 1).color).toEqual([255, 255, 255])
  })
})

describe('the seam crossing', () => {
  it('writes the bar values as gradient.rs serde reads them, and reads them back', () => {
    const params = {
      kind: 'radial',
      colors: [{ at: 0, color: '#102030' }, { at: 1, color: '#ffffff' }],
      opacities: [{ at: 0, opacity: 100 }, { at: 1, opacity: 40 }],
      reverse: true,
      dither: false,
    }
    const wire = wireGradient(params, { x: 10.12345, y: 20 }, { x: 60, y: 80 })
    expect(wire).toEqual({
      kind: 'radial',
      start: [10.123, 20],
      end: [60, 80],
      colors: [{ at: 0, color: [16, 32, 48] }, { at: 1, color: [255, 255, 255] }],
      opacities: [{ at: 0, opacity: 1 }, { at: 1, opacity: 0.4 }],
      reverse: true,
      dither: false,
    })
    const back = fromWire(wire)
    expect(back.params).toEqual(params)
    expect(back.start).toEqual({ x: 10.123, y: 20 })
    expect(back.end).toEqual({ x: 60, y: 80 })
  })

  it('falls back to black to white, dithered, when the bar holds no stops', () => {
    const wire = wireGradient({}, { x: 0, y: 0 }, { x: 1, y: 1 })
    expect(wire.kind).toBe('linear')
    expect(wire.dither).toBe(true)
    expect(fromWire(wire).params.colors).toEqual(DEFAULT_STOPS.colors)
  })

  it('puts the line in page pixels for the preview', () => {
    const g = inPixels(wireGradient({}, { x: 50, y: 25 }, { x: 100, y: 100 }), 800, 1200)
    expect(g.start).toEqual([400, 300])
    expect(g.end).toEqual([800, 1200])
  })
})

describe('snapped', () => {
  it('turns a drag to the nearest 15 degrees on the page, keeping its length', () => {
    // 100 by 8 page pixels on a square page: 4.6 degrees, which snaps flat.
    const end = snapped({ x: 0, y: 0 }, { x: 10, y: 0.8 }, 1000, 1000)
    expect(end.x).toBeCloseTo(Math.hypot(10, 0.8))
    expect(end.y).toBeCloseTo(0)
  })

  it('measures the angle in pixels, not percent, on a tall page', () => {
    // 10% across and 5% down a 1000 by 2000 page is 45 degrees in pixels.
    const end = snapped({ x: 0, y: 0 }, { x: 10, y: 5 }, 1000, 2000)
    expect(end.x * 10).toBeCloseTo(end.y * 20)
  })

  it('leaves a drag of no length where it is', () => {
    expect(snapped({ x: 5, y: 5 }, { x: 5, y: 5 }, 100, 100)).toEqual({ x: 5, y: 5 })
  })
})

describe('the stops', () => {
  it('builds the two presets from the brush colour', () => {
    expect(presetStops('foregroundToBackground', '#202020').colors).toEqual([{ at: 0, color: '#202020' }, { at: 1, color: '#ffffff' }])
    expect(presetStops('foregroundToBackground', '#f0f0f0').colors[1].color).toBe('#000000')
    expect(presetStops('foregroundToBackground', '#ff0000', '#00ff00').colors[1].color).toBe('#00ff00')
    expect(presetStops('foregroundToBackground', '#202020', '#202020').colors[1].color).toBe('#ffffff')
    expect(presetStops('foregroundToTransparent', '#202020')).toEqual({
      colors: [{ at: 0, color: '#202020' }, { at: 1, color: '#202020' }],
      opacities: [{ at: 0, opacity: 100 }, { at: 1, opacity: 0 }],
    })
  })

  it('adds a stop half way into the widest gap with the value already there', () => {
    const colors = [{ at: 0, color: '#000000' }, { at: 0.2, color: '#333333' }, { at: 1, color: '#ffffff' }]
    const added = addStop(colors, (at) => ({ at, color: colorBetween(colors, at) }))
    expect(added.map((stop) => stop.at)).toEqual([0, 0.2, 0.6, 1])
    expect(added[2].color).toBe(colorBetween(colors, 0.6))
    expect(opacityBetween([{ at: 0, opacity: 100 }, { at: 1, opacity: 0 }], 0.25)).toBe(75)
  })
})

describe('paintGradient', () => {
  it('fills an image covering the box, at the given scale', () => {
    const image = { data: new Uint8ClampedArray(2 * 1 * 4), width: 2, height: 1 }
    const g = { ...wireGradient({}, { x: 0, y: 0 }, { x: 0, y: 0 }), start: [0, 0], end: [100, 0] }
    // Two image pixels over 100 page pixels: their centres sit at 25 and 75.
    paintGradient(/** @type {any} */ (image), g, { x: 0, y: 0 }, 0.02)
    expect([...image.data]).toEqual([64, 64, 64, 255, 191, 191, 191, 255])
  })
})
