/**
 * The arithmetic behind a layer drag and turn: client pixels to page pixels
 * at any zoom, the page-edge stop, and the angle swept about the pivot.
 */
import { describe, expect, it } from 'vitest'

import {
  NUDGE_LARGE,
  TURN_LARGE,
  angleAt,
  clampOffset,
  clientPointToPage,
  nudgeFor,
  turnFor,
  turnedRotation,
} from './layertransform.js'

const page = { width: 1600, height: 2400 }

describe('a pointer in client pixels', () => {
  it('lands on the same page pixel at every zoom', () => {
    // The same point a quarter across and a sixth down, at 25%, 50%, 100% and 200%.
    for (const zoom of [0.25, 0.5, 1, 2]) {
      const sheet = { left: 40, top: 70, width: 1600 * zoom, height: 2400 * zoom }
      const point = clientPointToPage({ x: 40 + 400 * zoom, y: 70 + 400 * zoom }, sheet, page)
      expect(point?.x).toBeCloseTo(400)
      expect(point?.y).toBeCloseTo(400)
    }
  })

  it('stays on its page pixel when the sheet scrolls or zooms under it', () => {
    // The page pixel under the pointer is read off the sheet as drawn now.
    const before = clientPointToPage({ x: 440, y: 470 }, { left: 40, top: 70, width: 1600, height: 2400 }, page)
    const scrolled = clientPointToPage({ x: 440, y: 270 }, { left: 40, top: -130, width: 1600, height: 2400 }, page)
    expect(scrolled).toEqual(before)
  })

  it('reads nothing off a sheet that has not been laid out', () => {
    expect(clientPointToPage({ x: 10, y: 10 }, { left: 0, top: 0, width: 0, height: 0 }, page)).toBeNull()
  })
})

describe('the page edge', () => {
  // 160×240 px at the top-left corner: its centre is at (160, 240).
  const source = { x: 5, y: 5, w: 10, h: 10 }

  it('stops the centre on the page, whichever edge the drag runs to', () => {
    expect(clampOffset(source, { x: -5000, y: 0 }, page)).toEqual({ x: -160, y: 0 })
    expect(clampOffset(source, { x: 5000, y: 0 }, page)).toEqual({ x: 1440, y: 0 })
    expect(clampOffset(source, { x: 0, y: -5000 }, page)).toEqual({ x: 0, y: -240 })
    expect(clampOffset(source, { x: 0, y: 5000 }, page)).toEqual({ x: 0, y: 2160 })
  })

  it('leaves a drag inside the page alone, in whole pixels', () => {
    expect(clampOffset(source, { x: 12.6, y: -3.2 }, page)).toEqual({ x: 13, y: -3 })
  })

  it('lets a longstrip layer reach into the next page and no further', () => {
    expect(clampOffset(source, { x: 0, y: 99999 }, page, { minY: 0, maxY: 200 })).toEqual({ x: 0, y: 4560 })
    expect(clampOffset(source, { x: 0, y: -99999 }, page, { minY: -100, maxY: 100 })).toEqual({ x: 0, y: -2640 })
  })
})

describe('a turn about the pivot', () => {
  const pivot = { x: 100, y: 100 }

  it('measures clockwise, as the page does', () => {
    expect(angleAt({ x: 200, y: 100 }, pivot)).toBe(0)
    expect(angleAt({ x: 100, y: 200 }, pivot)).toBe(90)
  })

  it('adds the swept angle to where the layer started', () => {
    // From the handle above the pivot round to its right: a quarter turn.
    expect(turnedRotation({ start: 0, pivot, from: { x: 100, y: 40 }, to: { x: 160, y: 100 } })).toBe(90)
    expect(turnedRotation({ start: 30, pivot, from: { x: 100, y: 40 }, to: { x: 40, y: 100 } })).toBe(-60)
  })

  it('wraps through half a turn and snaps with Shift', () => {
    expect(turnedRotation({ start: 170, pivot, from: { x: 200, y: 100 }, to: { x: 100, y: 200 } })).toBe(-100)
    expect(turnedRotation({ start: 0, pivot, from: { x: 200, y: 100 }, to: { x: 200, y: 122 }, snap: true })).toBe(15)
  })
})

describe('keys', () => {
  it('nudge a pixel, or ten with Shift', () => {
    expect(nudgeFor('ArrowLeft', false)).toEqual({ x: -1, y: 0 })
    expect(nudgeFor('ArrowDown', true)).toEqual({ x: 0, y: NUDGE_LARGE })
    expect(nudgeFor('a', false)).toBeNull()
  })

  it('turn the slider a degree, fifteen with Shift or a page key', () => {
    expect(turnFor('ArrowRight', false, 10)).toBe(11)
    expect(turnFor('ArrowLeft', true, 10)).toBe(10 - TURN_LARGE)
    expect(turnFor('PageUp', false, 175)).toBe(-170)
    expect(turnFor('Enter', false, 42)).toBeNull()
  })

  it('go to the ends of the slider with Home and End, and 0 stands it upright', () => {
    // The handle says it runs from -180 to 180, so Home and End go there.
    expect(turnFor('Home', false, 42)).toBe(-180)
    expect(turnFor('End', false, 42)).toBe(180)
    expect(turnFor('0', false, 42)).toBe(0)
  })
})
