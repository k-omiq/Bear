/**
 * The gesture-to-geometry arithmetic. Core logic: everything here is either
 * right or subtly wrong, and none of it imports Svelte or touches the DOM.
 * The components that call it are not tested, per the standing instruction.
 */

import { describe, expect, it } from 'vitest'
import {
  MIN_SPAN,
  PAGE_SPAN,
  boundsOf,
  brushRadius,
  centredBbox,
  clampBbox,
  clientCentre,
  cloneOffset,
  detectAreaAround,
  draftKeyIntent,
  menuPoint,
  moveBbox,
  paintedStroke,
  pointIn,
  polylinePoints,
  rectBetween,
  regionAt,
  resizeBbox,
  shouldStamp,
  stripBoundsOf,
  stripRectBetween,
} from './gesture.js'

const RECT = { left: 100, top: 50, width: 400, height: 600 }

describe('pointIn', () => {
  it('maps a client point onto the page as a percentage of the drawn box', () => {
    expect(pointIn(300, 350, RECT)).toEqual({ x: 50, y: 50, p: 0.5 })
    expect(pointIn(100, 50, RECT)).toEqual({ x: 0, y: 0, p: 0.5 })
    expect(pointIn(500, 650, RECT)).toEqual({ x: 100, y: 100, p: 0.5 })
  })

  it('records pointer pressure when positive and defaults to 0.5', () => {
    expect(pointIn(300, 350, RECT, 0.75)).toEqual({ x: 50, y: 50, p: 0.75 })
    expect(pointIn(300, 350, RECT, 0)).toEqual({ x: 50, y: 50, p: 0.5 })
    expect(pointIn(300, 350, RECT, undefined)).toEqual({ x: 50, y: 50, p: 0.5 })
  })

  it('is scale-independent - the same content point at two zooms', () => {
    const zoomed = { left: 100, top: 50, width: 800, height: 1200 }
    expect(pointIn(300, 350, RECT)).toEqual(pointIn(500, 650, zoomed))
  })

  it('clamps a pointer that left the page', () => {
    expect(pointIn(-500, -500, RECT)).toEqual({ x: 0, y: 0, p: 0.5 })
    expect(pointIn(5000, 5000, RECT)).toEqual({ x: 100, y: 100, p: 0.5 })
  })

  it('keeps anchor-relative coordinates past a longstrip join when requested', () => {
    const point = pointIn(300, 710, RECT, 0.5, false)
    expect(point).toMatchObject({ x: 50, p: 0.5 })
    expect(point.y).toBeCloseTo(110)
  })

  it('survives a zero-sized box rather than dividing by it', () => {
    const point = pointIn(10, 10, { left: 0, top: 0, width: 0, height: 0 })
    expect(Number.isFinite(point.x)).toBe(true)
    expect(Number.isFinite(point.y)).toBe(true)
    expect(point.p).toBe(0.5)
  })
})

describe('continuous strip geometry', () => {
  it('keeps a rectangle on both sides of a join', () => {
    expect(stripRectBetween({ x: 20, y: 95 }, { x: 70, y: 108 })).toEqual({
      x: 20, y: 95, w: 50, h: 13,
    })
  })

  it('keeps stroke bounds past the anchor page', () => {
    expect(stripBoundsOf([{ x: 40, y: 98 }, { x: 50, y: 104 }], { rx: 2, ry: 3 })).toEqual({
      x: 38, y: 95, w: 14, h: 12,
    })
  })
})

describe('clampBbox', () => {
  it('gives a zero-area box the floor', () => {
    expect(clampBbox({ x: 10, y: 10, w: 0, h: 0 })).toEqual({
      x: 10,
      y: 10,
      w: MIN_SPAN,
      h: MIN_SPAN,
    })
  })

  it('pulls a box that overhangs the page back on, whole', () => {
    const box = clampBbox({ x: 95, y: 96, w: 20, h: 20 })
    expect(box.x + box.w).toBeLessThanOrEqual(100)
    expect(box.y + box.h).toBeLessThanOrEqual(100)
    expect(box.w).toBe(20)
  })

  it('never yields a negative origin', () => {
    expect(clampBbox({ x: -30, y: -30, w: 10, h: 10 })).toMatchObject({ x: 0, y: 0 })
  })
})

describe('rectBetween', () => {
  it('is the same box in either drag direction', () => {
    const a = rectBetween({ x: 10, y: 10 }, { x: 40, y: 60 })
    const b = rectBetween({ x: 40, y: 60 }, { x: 10, y: 10 })
    expect(a).toEqual(b)
    expect(a).toEqual({ x: 10, y: 10, w: 30, h: 50 })
  })

  it('gives a click-without-drag the floor rather than nothing', () => {
    expect(rectBetween({ x: 20, y: 20 }, { x: 20, y: 20 })).toMatchObject({
      w: MIN_SPAN,
      h: MIN_SPAN,
    })
  })
})

describe('boundsOf', () => {
  it('is the run of points, grown by the stroke radius on each axis', () => {
    const box = boundsOf([{ x: 20, y: 30 }, { x: 40, y: 50 }], { rx: 2, ry: 1 })
    expect(box).toEqual({ x: 18, y: 29, w: 24, h: 22 })
  })

  it('gives a single stamp the footprint of the brush that made it', () => {
    expect(boundsOf([{ x: 50, y: 50 }], { rx: 3, ry: 2 })).toEqual({
      x: 47,
      y: 48,
      w: 6,
      h: 4,
    })
  })

  it('is null for an empty run', () => {
    expect(boundsOf([])).toBeNull()
    expect(boundsOf(null)).toBeNull()
  })

  it('stays on the page when a stroke runs to the edge', () => {
    const box = boundsOf([{ x: 0, y: 0 }, { x: 100, y: 100 }], { rx: 5, ry: 5 })
    expect(box).toEqual({ x: 0, y: 0, w: 100, h: 100 })
  })
})

describe('brushRadius', () => {
  it('converts page pixels to page percent, per axis', () => {
    expect(brushRadius(32, 1600, 2400)).toEqual({ rx: 1, ry: 32 / 2 / 2400 * 100 })
  })

  it('is the same ink at every zoom, because it never sees the zoom', () => {
    // The drawn sheet is what zoom changes, and it is not an argument here:
    // the only inputs are the brush and the page's own pixels. So a 40px brush
    // is 20 page pixels of radius on either page, however either is drawn -
    // even though the two answers, being percentages, differ.
    const tall = brushRadius(40, 800, 4000)
    const wide = brushRadius(40, 1600, 2400)
    expect((tall.rx / PAGE_SPAN) * 800).toBe(20)
    expect((tall.ry / PAGE_SPAN) * 4000).toBe(20)
    expect((wide.rx / PAGE_SPAN) * 1600).toBe(20)
    expect((wide.ry / PAGE_SPAN) * 2400).toBe(20)
    expect(tall.rx).not.toBe(wide.rx)
  })

  it('refuses to divide by a page of no width', () => {
    expect(Number.isFinite(brushRadius(20, 0, 0).rx)).toBe(true)
  })
})

describe('shouldStamp', () => {
  const radius = { rx: 2, ry: 2 }

  it('always stamps the first point', () => {
    expect(shouldStamp(null, { x: 10, y: 10 }, radius, 12)).toBe(true)
  })

  it('skips a point inside the spacing step and takes one beyond it', () => {
    expect(shouldStamp({ x: 10, y: 10 }, { x: 10.1, y: 10 }, radius, 12)).toBe(false)
    expect(shouldStamp({ x: 10, y: 10 }, { x: 12, y: 10 }, radius, 12)).toBe(true)
  })

  it('steps further for a wider brush at the same spacing', () => {
    const near = { x: 10.6, y: 10 }
    expect(shouldStamp({ x: 10, y: 10 }, near, { rx: 1, ry: 1 }, 20)).toBe(true)
    expect(shouldStamp({ x: 10, y: 10 }, near, { rx: 8, ry: 8 }, 20)).toBe(false)
  })
})

describe('regionAt', () => {
  const regions = [
    { id: 'under', bbox: { x: 0, y: 0, w: 50, h: 50 } },
    { id: 'over', bbox: { x: 10, y: 10, w: 10, h: 10 } },
  ]

  it('returns the last region containing the point, matching what is drawn on top', () => {
    expect(regionAt({ x: 15, y: 15 }, regions)?.id).toBe('over')
    expect(regionAt({ x: 40, y: 40 }, regions)?.id).toBe('under')
  })

  it('returns null over empty page', () => {
    expect(regionAt({ x: 90, y: 90 }, regions)).toBeNull()
  })
})

describe('the keyboard draft', () => {
  it('starts centred', () => {
    expect(centredBbox(20, 10)).toEqual({ x: 40, y: 45, w: 20, h: 10 })
  })

  it('moves by the step and stops at the edge', () => {
    expect(moveBbox({ x: 10, y: 10, w: 5, h: 5 }, 1, -1)).toMatchObject({ x: 11, y: 9 })
    expect(moveBbox({ x: 0, y: 0, w: 5, h: 5 }, -10, -10)).toMatchObject({ x: 0, y: 0 })
    expect(moveBbox({ x: 95, y: 0, w: 5, h: 5 }, 10, 0)).toMatchObject({ x: 95 })
  })

  it('resizes from the top-left and never below the floor', () => {
    expect(resizeBbox({ x: 10, y: 10, w: 5, h: 5 }, 2, 3)).toMatchObject({ x: 10, w: 7, h: 8 })
    expect(resizeBbox({ x: 10, y: 10, w: 5, h: 5 }, -50, -50)).toMatchObject({
      w: MIN_SPAN,
      h: MIN_SPAN,
    })
  })
})

describe('draftKeyIntent', () => {
  const open = { hasDraft: false }
  const drafting = { hasDraft: true }

  it('activates to open a draft and again to commit it', () => {
    expect(draftKeyIntent({ key: 'Enter' }, open)).toEqual({ kind: 'open' })
    expect(draftKeyIntent({ key: ' ' }, open)).toEqual({ kind: 'open' })
    expect(draftKeyIntent({ key: 'Enter' }, drafting)).toEqual({ kind: 'commit' })
  })

  it('moves on an arrow and resizes on Shift-arrow', () => {
    expect(draftKeyIntent({ key: 'ArrowLeft' }, drafting)).toEqual({ kind: 'move', dx: -1, dy: 0 })
    expect(draftKeyIntent({ key: 'ArrowDown', shiftKey: true }, drafting)).toEqual({
      kind: 'resize',
      dx: 0,
      dy: 1,
    })
  })

  it('scales by the step', () => {
    expect(draftKeyIntent({ key: 'ArrowRight' }, { hasDraft: true, step: 5 })).toEqual({
      kind: 'move',
      dx: 5,
      dy: 0,
    })
  })

  it('ignores arrows with no draft open, so the chapter still pages', () => {
    expect(draftKeyIntent({ key: 'ArrowLeft' }, open)).toBeNull()
  })

  it('samples the clone source only for a tool that has one', () => {
    expect(draftKeyIntent({ key: 's' }, { hasDraft: false, cloneCapable: true })).toEqual({
      kind: 'sample',
    })
    expect(draftKeyIntent({ key: 'S' }, { hasDraft: true, cloneCapable: true })).toEqual({
      kind: 'sample',
    })
    expect(draftKeyIntent({ key: 's' }, drafting)).toBeNull()
  })

  it('leaves every platform chord alone', () => {
    expect(draftKeyIntent({ key: 'Enter', metaKey: true }, open)).toBeNull()
    expect(draftKeyIntent({ key: 'ArrowLeft', ctrlKey: true }, drafting)).toBeNull()
  })

  it('claims nothing else - Escape included, which the editor owns', () => {
    expect(draftKeyIntent({ key: 'Escape' }, drafting)).toBeNull()
    expect(draftKeyIntent({ key: 'x' }, drafting)).toBeNull()
    expect(draftKeyIntent(null, drafting)).toBeNull()
  })
})

describe('cloneOffset', () => {
  const source = { x: 30, y: 30 }

  it('is nothing at all until a source has been sampled', () => {
    expect(cloneOffset({ source: null, strokeStart: { x: 1, y: 1 }, alignment: 'aligned', offset: null })).toBeNull()
  })

  it('anchors on the sampled point for the first stroke, whatever the alignment', () => {
    const first = cloneOffset({ source, strokeStart: { x: 50, y: 40 }, alignment: 'aligned', offset: null })
    expect(first).toEqual({ offset: { x: -20, y: -10 }, source })
  })

  it('aligned keeps the offset, so the next stroke reads on from the last', () => {
    const next = cloneOffset({
      source,
      strokeStart: { x: 60, y: 60 },
      alignment: 'aligned',
      offset: { x: -20, y: -10 },
    })
    expect(next).toEqual({ offset: { x: -20, y: -10 }, source: { x: 40, y: 50 } })
  })

  it('nonAligned re-anchors on the sampled point every stroke', () => {
    const next = cloneOffset({
      source,
      strokeStart: { x: 60, y: 60 },
      alignment: 'nonAligned',
      offset: { x: -20, y: -10 },
    })
    expect(next).toEqual({ offset: { x: -30, y: -30 }, source })
  })
})

describe('polylinePoints', () => {
  it('serialises page-percent points, rounded', () => {
    expect(polylinePoints([{ x: 1.239, y: 2 }, { x: 3, y: 4.5 }])).toBe('1.24,2 3,4.5')
  })

  it('is empty for no points', () => {
    expect(polylinePoints([])).toBe('')
    expect(polylinePoints(null)).toBe('')
  })
})

/**
 * The shape half of the seam. What
 * matters is that the **path** survives - an L and the square around it have
 * the same bounds, so a bbox is not evidence of anything - and that the radius
 * is the brush's own half-diameter, in page pixels.
 */
describe('paintedStroke', () => {
  const L = [
    { x: 20, y: 20 },
    { x: 20, y: 60 },
    { x: 60, y: 60 },
  ]

  it('keeps every point of the path and halves the diameter', () => {
    const stroke = paintedStroke(L, 40)
    expect(stroke.points).toEqual(L)
    expect(stroke.radius).toBe(20)
  })

  it('rounds the path the way the preview does, and no further', () => {
    const stroke = paintedStroke([{ x: 1.2394, y: 2 }], 10)
    expect(stroke.points).toEqual([{ x: 1.24, y: 2 }])
  })

  it('drops points that are not points', () => {
    const stroke = paintedStroke([{ x: 1, y: 2 }, null, { x: NaN, y: 3 }], 8)
    expect(stroke.points).toEqual([{ x: 1, y: 2 }])
  })

  it('is null where there is no path at all', () => {
    expect(paintedStroke([], 40)).toBe(null)
    expect(paintedStroke(null, 40)).toBe(null)
    expect(paintedStroke([null], 40)).toBe(null)
  })

  it('floors the radius rather than describing a stroke of no width', () => {
    expect(paintedStroke(L, 0).radius).toBe(0.5)
    expect(paintedStroke(L, -20).radius).toBe(0.5)
    expect(paintedStroke(L, undefined).radius).toBe(0.5)
  })
})

describe('menuPoint', () => {
  it('opens where the pointer is', () => {
    expect(menuPoint({ clientX: 320, clientY: 210 }, RECT)).toEqual({ x: 320, y: 210 })
  })

  it('falls back to the middle of the element for a keyboard-raised menu', () => {
    // Shift+F10 and the context-menu key raise `contextmenu` with no position.
    expect(menuPoint({ clientX: 0, clientY: 0 }, RECT)).toEqual({ x: 300, y: 350 })
    expect(menuPoint({}, RECT)).toEqual({ x: 300, y: 350 })
  })

  it('keeps a position that is only half zero - the top or left edge of the window', () => {
    expect(menuPoint({ clientX: 0, clientY: 210 }, RECT)).toEqual({ x: 0, y: 210 })
    expect(menuPoint({ clientX: 320, clientY: 0 }, RECT)).toEqual({ x: 320, y: 0 })
  })

  it('is the origin when there is nothing to measure', () => {
    expect(menuPoint({ clientX: 0, clientY: 0 }, null)).toEqual({ x: 0, y: 0 })
  })
})

describe('detectAreaAround', () => {
  it('is a box around the point, square in page pixels', () => {
    // 16% of 1600 px is 256 px, which is 10.67% of 2400 px.
    const area = detectAreaAround({ x: 50, y: 50 }, 1600, 2400)
    expect(area.x).toBeCloseTo(42)
    expect(area.w).toBeCloseTo(16)
    expect(area.h).toBeCloseTo(10.6667, 3)
    expect(area.y).toBeCloseTo(50 - area.h / 2)
  })

  it('moves back inside the page near an edge', () => {
    expect(detectAreaAround({ x: 1, y: 99 }, 1000, 1000)).toEqual({ x: 0, y: 84, w: 16, h: 16 })
  })

  it('is never taller than the page', () => {
    expect(detectAreaAround({ x: 50, y: 50 }, 4000, 400)).toEqual({ x: 42, y: 0, w: 16, h: 100 })
  })
})

describe('clientCentre', () => {
  it('is pointIn run backwards, through the same measured box', () => {
    const bbox = { x: 25, y: 40, w: 50, h: 20 }
    const centre = clientCentre(bbox, RECT)
    expect(centre).toEqual({ x: 300, y: 350 })
    expect(pointIn(centre.x, centre.y, RECT)).toEqual({ x: 50, y: 50, p: 0.5 })
  })

  it('lands inside the region it describes', () => {
    const bbox = { x: 0, y: 0, w: 10, h: 10 }
    const centre = clientCentre(bbox, RECT)
    expect(regionAt(pointIn(centre.x, centre.y, RECT), [{ id: 'r1', bbox }])?.id).toBe('r1')
  })
})
