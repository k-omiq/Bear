import { describe, expect, it } from 'vitest'
import { gridAxis, gridCovers } from './pixelgrid.js'

describe('gridAxis', () => {
  it('puts every line on its own image pixel, at a zoom that is not a whole number', () => {
    // 3969% on a 2x screen, a thousand image pixels into the page: a
    // repeated 39.5px square is 190 CSS pixels off here.
    const pixel = 39.69
    const dpr = 2
    const edge = -1000 * pixel + 300
    const axis = gridAxis({ edge, pixel, count: 1600, view: 1400, dpr, overscan: 0 })
    expect(axis).not.toBeNull()
    expect(axis.lines.length).toBeGreaterThan(30)
    for (const at of axis.lines) {
      const exact = (axis.start + at - edge * dpr) / (pixel * dpr)
      expect(Math.abs(exact - Math.round(exact)) * pixel * dpr).toBeLessThanOrEqual(0.5 + 1e-6)
    }
    // One line for each image pixel, and no two on the same one.
    const steps = axis.lines.slice(1).map((at, index) => at - axis.lines[index])
    expect(Math.min(...steps)).toBe(79)
    expect(Math.max(...steps)).toBe(80)
    // Image pixel 1000 starts 300 CSS pixels into the view.
    expect(axis.lines).toContain(600 - axis.start)
  })

  it('draws no more than the view and its overscan', () => {
    const axis = gridAxis({ edge: -50_000, pixel: 128, count: 2000, view: 1000, dpr: 2, overscan: 256 })
    expect(axis.start).toBe(-512)
    expect(axis.length).toBe((1000 + 2 * 256) * 2)
    expect(axis.seenFrom).toBe(0)
    expect(axis.seenTo).toBe(2000)
  })

  it('stops at the ends of the sheet, with a line at the first pixel and none past the last', () => {
    const axis = gridAxis({ edge: 100, pixel: 16, count: 10, view: 1000, dpr: 1 })
    expect(axis.start).toBe(100)
    expect(axis.length).toBe(160)
    expect(axis.lines).toEqual([0, 16, 32, 48, 64, 80, 96, 112, 128, 144])
  })

  it('is null for a sheet nowhere near the view', () => {
    expect(gridAxis({ edge: 5000, pixel: 16, count: 100, view: 1000, dpr: 2 })).toBeNull()
    expect(gridAxis({ edge: -9000, pixel: 16, count: 100, view: 1000, dpr: 2 })).toBeNull()
    expect(gridAxis({ edge: 0, pixel: 0, count: 100, view: 1000, dpr: 2 })).toBeNull()
  })

  it('starts on the device pixel the browser starts the page on', () => {
    // A sheet at 20.25 CSS pixels on a 2x screen is drawn from device pixel 41.
    const axis = gridAxis({ edge: 20.25, pixel: 39.69, count: 1600, view: 1400, dpr: 2 })
    expect(axis.start).toBe(41)
    expect(axis.lines[0]).toBe(0)
    expect(axis.lines[1]).toBe(79)
  })
})

describe('gridCovers', () => {
  it('holds while a scroll stays inside the overscan, and not past it', () => {
    const spec = { pixel: 64, count: 4000, view: 1000, dpr: 2, overscan: 256 }
    const before = gridAxis({ ...spec, edge: -10_000 })
    const drawn = { from: before.start + 20_000, to: before.start + before.length + 20_000 }
    const near = gridAxis({ ...spec, edge: -10_200 })
    expect(gridCovers(drawn, near, -20_400)).toBe(true)
    const far = gridAxis({ ...spec, edge: -10_300 })
    expect(gridCovers(drawn, far, -20_600)).toBe(false)
    expect(gridCovers(null, near, -20_400)).toBe(false)
  })
})
