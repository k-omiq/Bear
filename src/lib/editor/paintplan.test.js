import { describe, expect, it } from 'vitest'
import {
  MAX_SPACING_PERCENT,
  MIN_RADIUS,
  boundsOfDabs,
  brushFromParams,
  nativePoints,
  planStroke,
  spacingPx,
} from './paintplan.js'

/**
 * These mirror `crates/cleaner-core/src/paint/plan.rs`'s own `#[cfg(test)]`
 * block, case for case and number for number. That is the point of them: the
 * two planners are the two halves of *parity of the plan*, and a preview
 * that plans a different stroke than the commit is a preview that lies.
 * Where a case here has no Rust twin it is because the coordinate
 * conversion is this side's alone.
 */

/** @type {import('./paintplan.js').BrushSpec} */
const DEFAULT = {
  size: 24,
  hardness: 50,
  flow: 100,
  opacity: 100,
  spacing: 10,
  pressureSize: true,
  pressureOpacity: false,
}

/**
 * @param {number} n
 * @param {number} dx
 */
function line(n, dx) {
  return Array.from({ length: n }, (_, i) => ({ x: i * dx, y: 0, p: 0.5 }))
}

describe('planStroke', () => {
  it('is no dabs for no points and one dab for one point', () => {
    expect(planStroke([], DEFAULT)).toEqual([])
    expect(planStroke(line(1, 0), DEFAULT)).toHaveLength(1)
  })

  it('lands a dab every spacing percent of a diameter', () => {
    const brush = { ...DEFAULT, size: 24, spacing: 10 }
    const step = spacingPx(brush)
    expect(step).toBeCloseTo(2.4, 12)

    const dabs = planStroke([{ x: 0, y: 0, p: 0.5 }, { x: 100, y: 0, p: 0.5 }], brush)
    expect(dabs).toHaveLength(1 + Math.floor(100 / step))
    for (let i = 1; i < dabs.length; i += 1) {
      const gap = Math.hypot(dabs[i].x - dabs[i - 1].x, dabs[i].y - dabs[i - 1].y)
      expect(Math.abs(gap - step)).toBeLessThan(1e-9)
    }
  })

  it('measures spacing along the whole polyline, not per segment', () => {
    const brush = { ...DEFAULT, size: 24, spacing: 10 }
    const many = planStroke(line(11, 1), brush)
    const one = planStroke([{ x: 0, y: 0, p: 0.5 }, { x: 10, y: 0, p: 0.5 }], brush)
    expect(many).toHaveLength(one.length)
    many.forEach((dab, index) => expect(Math.abs(dab.x - one[index].x)).toBeLessThan(1e-9))
  })

  it('maps pressure to radius and alpha only where it is enabled', () => {
    const both = { ...DEFAULT, size: 20, flow: 80, pressureSize: true, pressureOpacity: true }
    const soft = planStroke([{ x: 0, y: 0, p: 0.25 }], both)
    expect(soft[0].radius).toBe(2.5)
    expect(soft[0].alpha).toBeCloseTo(0.2, 12)

    const neither = { ...both, pressureSize: false, pressureOpacity: false }
    const flat = planStroke([{ x: 0, y: 0, p: 0.25 }], neither)
    expect(flat[0].radius).toBe(10)
    expect(flat[0].alpha).toBeCloseTo(0.8, 12)

    // A pressure of zero on a pressure-size brush is still a dab.
    expect(planStroke([{ x: 0, y: 0, p: 0 }], both)[0].radius).toBe(MIN_RADIUS)
  })

  it('plans the same stroke to the same dabs every time', () => {
    const brush = { ...DEFAULT, spacing: 7 }
    const points = line(20, 3.5)
    expect(planStroke(points, brush)).toEqual(planStroke(points, brush))
  })

  it('clamps spacing rather than trusting it', () => {
    const brush = { ...DEFAULT, size: 100 }
    expect(spacingPx({ ...brush, spacing: 0 })).toBe(1)
    expect(spacingPx({ ...brush, spacing: -5 })).toBe(1)
    expect(spacingPx({ ...brush, spacing: 900 })).toBe(MAX_SPACING_PERCENT)
    expect(spacingPx({ ...brush, spacing: NaN })).toBe(1)
  })

  it('reads pressure verbatim, and only invents one where there is none', () => {
    const brush = { ...DEFAULT, size: 20, pressureSize: true }
    expect(planStroke([{ x: 0, y: 0 }], brush)[0].radius).toBe(5)
    // A real zero is a real zero here: the seam's fold-to-a-half happens one
    // step earlier, in `nativePoints`.
    expect(planStroke([{ x: 0, y: 0, p: 0 }], brush)[0].radius).toBe(MIN_RADIUS)
  })

  it('is prefix-stable, which is what lets the preview draw incrementally', () => {
    const brush = { ...DEFAULT, size: 24, spacing: 10 }
    const full = line(9, 4)
    const whole = planStroke(full, brush)
    const partial = planStroke(full.slice(0, 5), brush)
    expect(partial.length).toBeLessThanOrEqual(whole.length)
    partial.forEach((dab, index) => expect(dab).toEqual(whole[index]))
  })

  it('ignores a zero-length segment rather than stalling on it', () => {
    const brush = { ...DEFAULT, size: 24, spacing: 10 }
    const doubled = planStroke(
      [{ x: 0, y: 0, p: 0.5 }, { x: 0, y: 0, p: 0.5 }, { x: 10, y: 0, p: 0.5 }],
      brush,
    )
    expect(doubled).toEqual(planStroke([{ x: 0, y: 0, p: 0.5 }, { x: 10, y: 0, p: 0.5 }], brush))
  })
})

describe('boundsOfDabs', () => {
  it('covers every dab reach, not just their centres', () => {
    const brush = { ...DEFAULT, size: 10, pressureSize: false }
    const dabs = planStroke([{ x: 20, y: 30, p: 1 }, { x: 60, y: 30, p: 1 }], brush)
    expect(boundsOfDabs(dabs, 2)).toEqual({ x0: 13, y0: 23, x1: 67, y1: 37 })
    expect(boundsOfDabs([], 2)).toBeNull()
  })
})

describe('nativePoints', () => {
  it('scales the two axes independently, because a sheet is not square', () => {
    const points = nativePoints([{ x: 50, y: 25, p: 0.4 }], 1600, 2400)
    expect(points).toEqual([{ x: 800, y: 600, p: 0.4 }])
  })

  it('applies the seam pressure convention, a reported zero included', () => {
    expect(nativePoints([{ x: 0, y: 0 }], 100, 100)[0].p).toBe(0.5)
    expect(nativePoints([{ x: 0, y: 0, p: 0 }], 100, 100)[0].p).toBe(0.5)
    expect(nativePoints([{ x: 0, y: 0, p: 0.3 }], 100, 100)[0].p).toBe(0.3)
  })

  it('drops points that are not points', () => {
    // @ts-expect-error deliberately malformed
    expect(nativePoints([null, { x: NaN, y: 0 }, { x: 1, y: 1 }], 100, 100)).toHaveLength(1)
  })
})

describe('brushFromParams', () => {
  it('reads the tool parameters the seam already sends', () => {
    expect(brushFromParams({ size: 28, hardness: 70, flow: 60, opacity: 80, spacing: 12 })).toEqual({
      size: 28,
      hardness: 70,
      flow: 60,
      opacity: 80,
      spacing: 12,
      pressureSize: true,
      pressureOpacity: false,
      seed: 0,
    })
  })

  it('clamps what the parameters can express past what the planner accepts', () => {
    const brush = brushFromParams({ size: 0, spacing: 900, flow: -20, opacity: 500 })
    expect(brush.size).toBe(1)
    expect(brush.spacing).toBe(MAX_SPACING_PERCENT)
    expect(brush.flow).toBe(0)
    expect(brush.opacity).toBe(100)
  })

  it('takes the clone stamp size from the tool that has no paint defaults', () => {
    expect(brushFromParams({ size: 32, opacity: 90, flow: 75, hardness: 60 }).size).toBe(32)
  })
})
