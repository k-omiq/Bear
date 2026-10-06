/**
 * Pointer and key arithmetic for moving and turning a layer on the canvas.
 *
 * Pure - no Svelte, no DOM, no state. The one measurement it takes is the
 * sheet's own drawn rectangle, for the reason `gesture.js` gives: the sheet is
 * laid out at the zoom already, so its box *is* the drawn scale. Dividing by
 * it is what makes a drag cover the same page pixels at 40% as at 240%.
 *
 * Offsets are **page pixels**, the unit `LayerStyle` stores; angles are
 * degrees, clockwise, in the `(-180, 180]` the native side keeps.
 */

import { normalizeAngle } from '../model/layers.js'

/** Below this much pointer movement (CSS px) a press is a click, not a drag. */
export const DRAG_SLOP = 3

/** One arrow press, and one with Shift, in page pixels. */
export const NUDGE = 1
export const NUDGE_LARGE = 10

/** One arrow press on the turn handle, and one with Shift, in degrees. */
export const TURN = 1
export const TURN_LARGE = 15

/** The step a Shift-drag of the turn handle snaps to, in degrees. */
export const TURN_SNAP = 15

/**
 * @typedef {{left: number, top: number, width: number, height: number}} Rect
 * @typedef {{x: number, y: number}} Point
 */

/**
 * A client point as page pixels, through the sheet's drawn rectangle. A
 * gesture measures the sheet again for every point it reads, so a scroll or a
 * zoom in the middle of a drag moves nothing under the pointer: the gesture
 * lives in page pixels, and client pixels are only how a point arrives.
 *
 * @param {Point} point - client pixels
 * @param {Rect} sheet - the page's drawn rectangle, now
 * @param {{width?: number, height?: number}} page
 * @returns {Point|null} null over a sheet that has not been laid out
 */
export function clientPointToPage(point, sheet, page) {
  const width = Number(page?.width) || 0
  const height = Number(page?.height) || 0
  if (!(sheet?.width > 0) || !(sheet?.height > 0) || !width || !height) return null
  return {
    x: ((point.x - sheet.left) * width) / sheet.width,
    y: ((point.y - sheet.top) * height) / sheet.height,
  }
}

/**
 * An offset that keeps the layer's centre on the page, so a layer dragged to
 * an edge stops there rather than leaving the sheet, where nothing could grab
 * it again. A longstrip page lets the centre cross its joins as far as the
 * next page's own edge, which is the reach a stroke has too
 * (`stripMinY`/`stripMaxY`).
 *
 * @param {{x: number, y: number, w: number, h: number}} source - the untransformed box, page percent
 * @param {Point} offset - page pixels
 * @param {{width?: number, height?: number}} page
 * @param {{minY?: number, maxY?: number}} [reach] - vertical reach, page percent
 * @returns {Point} whole page pixels
 */
export function clampOffset(source, offset, page, reach = {}) {
  const width = Number(page?.width) || 0
  const height = Number(page?.height) || 0
  if (!width || !height) return { x: Math.round(offset.x), y: Math.round(offset.y) }
  const cx = ((source.x + source.w / 2) / 100) * width
  const cy = ((source.y + source.h / 2) / 100) * height
  const minY = ((reach.minY ?? 0) / 100) * height
  const maxY = ((reach.maxY ?? 100) / 100) * height
  const x = Math.min(width, Math.max(0, cx + offset.x)) - cx
  const y = Math.min(maxY, Math.max(minY, cy + offset.y)) - cy
  return { x: Math.round(x), y: Math.round(y) }
}

/**
 * The direction from the pivot to a point, in degrees, clockwise from the
 * x axis - client coordinates run downwards, and so does the page's.
 *
 * @param {Point} point
 * @param {Point} pivot
 */
export function angleAt(point, pivot) {
  return (Math.atan2(point.y - pivot.y, point.x - pivot.x) * 180) / Math.PI
}

/**
 * The rotation a turn-handle drag has reached: the layer's rotation when the
 * drag began plus the angle the pointer has swept about the pivot since.
 * Tenths of a degree, or whole `TURN_SNAP` steps with `snap`.
 *
 * @param {{start: number, pivot: Point, from: Point, to: Point, snap?: boolean}} spec
 */
export function turnedRotation({ start, pivot, from, to, snap = false }) {
  const swept = angleAt(to, pivot) - angleAt(from, pivot)
  const raw = normalizeAngle(start + swept)
  const stepped = snap ? Math.round(raw / TURN_SNAP) * TURN_SNAP : Math.round(raw * 10) / 10
  return normalizeAngle(stepped)
}

/**
 * The page-pixel step an arrow asks for, or null for any other key.
 *
 * @param {string} key
 * @param {boolean} large
 * @returns {Point|null}
 */
export function nudgeFor(key, large) {
  const step = large ? NUDGE_LARGE : NUDGE
  switch (key) {
    case 'ArrowLeft': return { x: -step, y: 0 }
    case 'ArrowRight': return { x: step, y: 0 }
    case 'ArrowUp': return { x: 0, y: -step }
    case 'ArrowDown': return { x: 0, y: step }
    default: return null
  }
}

/**
 * The rotation a key on the turn handle asks for, or null for any other key.
 * The handle is a slider from -180 to 180, so it answers the slider keys:
 * arrows by a degree, Page Up / Page Down by `TURN_LARGE`, Home to the
 * minimum and End to the maximum. `0` stands the layer upright again.
 *
 * @param {string} key
 * @param {boolean} large
 * @param {number} current
 * @returns {number|null}
 */
export function turnFor(key, large, current) {
  const step = large ? TURN_LARGE : TURN
  switch (key) {
    case 'ArrowRight':
    case 'ArrowUp': return normalizeAngle(current + step)
    case 'ArrowLeft':
    case 'ArrowDown': return normalizeAngle(current - step)
    case 'PageUp': return normalizeAngle(current + TURN_LARGE)
    case 'PageDown': return normalizeAngle(current - TURN_LARGE)
    case 'Home': return -180
    case 'End': return 180
    case '0': return 0
    default: return null
  }
}
