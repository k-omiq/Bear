/**
 * A layer's capabilities and placement, as the interface reads them.
 *
 * The native side decides both: `cleaner_core::patch::LayerCapabilities` is
 * derived from what produced a layer and crosses the seam as
 * `mask.capabilities`, and `Library::set_layer_style` refuses anything it does
 * not allow. This module is the interface's copy of the same rules, for two
 * readers only: the controls, which offer exactly what the capabilities say,
 * and the mock backend, which has no native side to ask. A mask that carries
 * `capabilities` is always read from them; the derivation below is the
 * fallback for a mask that does not (the mock, and saved test fixtures).
 *
 * Pure - no Svelte, no DOM, no state.
 */

import { currentRung } from './ladder.js'

/**
 * @typedef {{opacity: number, offsetX: number, offsetY: number, rotation: number, locked: boolean}} LayerStyle
 * @typedef {'movable'|'fixed'|'none'} LayerPlacement
 * @typedef {{transform: LayerPlacement, lock: boolean, opacity: boolean}} LayerCapabilities
 * @typedef {{x: number, y: number, w: number, h: number}} Bbox
 */

/** @type {Readonly<LayerStyle>} */
export const DEFAULT_LAYER = Object.freeze({ opacity: 100, offsetX: 0, offsetY: 0, rotation: 0, locked: false })

/** The native offset clamp, in page pixels (`LayerStyle::sanitized`). */
export const MAX_OFFSET = 10_000

/** @type {Readonly<LayerCapabilities>} */
const MOVABLE = Object.freeze({ transform: 'movable', lock: true, opacity: true })
/** @type {Readonly<LayerCapabilities>} */
const FIXED = Object.freeze({ transform: 'fixed', lock: false, opacity: true })
/** @type {Readonly<LayerCapabilities>} */
export const NO_OUTPUT = Object.freeze({ transform: 'none', lock: false, opacity: false })

/**
 * Given pixels move; everything read from where it sits does not
 * (`Engine::layer_capabilities`). A patch saved as the retired `denoise` rung
 * is a fill there, and here.
 */
const MOVABLE_ENGINES = new Set(['fill', 'paint'])

/**
 * What a region's layer may do.
 *
 * @param {{outcome?: string, mask?: any}|null|undefined} region
 * @returns {LayerCapabilities}
 */
export function layerCapabilities(region) {
  const mask = region?.mask
  if (!mask) return NO_OUTPUT
  if (mask.capabilities) return mask.capabilities
  if (region?.outcome === 'detected') return NO_OUTPUT
  return MOVABLE_ENGINES.has(currentRung(mask.provenance?.engine)) ? MOVABLE : FIXED
}

/**
 * The saved style with every field present.
 *
 * @param {{mask?: any}|null|undefined} region
 * @returns {LayerStyle}
 */
export function layerOf(region) {
  return { ...DEFAULT_LAYER, ...(region?.mask?.layer ?? {}) }
}

/**
 * Whether a drag or a turn may start on this layer now.
 *
 * @param {{outcome?: string, mask?: any}|null|undefined} region
 */
export function canTransform(region) {
  return layerCapabilities(region).transform === 'movable' && !layerOf(region).locked
}

/**
 * An angle in degrees, in `(-180, 180]`, the range the native side stores.
 *
 * @param {number} degrees
 * @returns {number}
 */
export function normalizeAngle(degrees) {
  if (!Number.isFinite(degrees)) return 0
  if (degrees >= -180 && degrees <= 180) return degrees === 0 ? 0 : degrees
  const wrapped = ((degrees % 360) + 360) % 360
  return wrapped > 180 ? wrapped - 360 : wrapped
}

/**
 * The style the native side would store for a request: whole-pixel offsets
 * within its clamp, a whole-percent opacity, a rotation in range.
 *
 * @param {Partial<LayerStyle>} layer
 * @returns {LayerStyle}
 */
export function sanitizeLayer(layer) {
  const full = { ...DEFAULT_LAYER, ...layer }
  const offset = (/** @type {number} */ value) =>
    Math.max(-MAX_OFFSET, Math.min(MAX_OFFSET, Math.round(Number(value) || 0)))
  return {
    opacity: Math.max(0, Math.min(100, Math.round(Number(full.opacity) || 0))),
    offsetX: offset(full.offsetX),
    offsetY: offset(full.offsetY),
    rotation: normalizeAngle(Number(full.rotation) || 0),
    locked: !!full.locked,
  }
}

/**
 * @param {LayerStyle} a
 * @param {LayerStyle} b
 */
export function samePlacement(a, b) {
  return a.offsetX === b.offsetX && a.offsetY === b.offsetY && a.rotation === b.rotation
}

/**
 * @param {LayerStyle} a
 * @param {LayerStyle} b
 */
export function sameLayer(a, b) {
  return samePlacement(a, b) && a.opacity === b.opacity && a.locked === b.locked
}

/**
 * `LayerStyle::edited`: the requested style, or the refusal's catalogue key
 * thrown as an `Error`, exactly as the command rejects it.
 *
 * @param {LayerStyle} current
 * @param {Partial<LayerStyle>} requested
 * @param {LayerCapabilities} capabilities
 * @returns {LayerStyle}
 */
export function editedLayer(current, requested, capabilities) {
  const now = sanitizeLayer(current)
  const next = sanitizeLayer(requested)
  if (capabilities.transform === 'none') {
    if (sameLayer(now, next)) return now
    throw new Error('masks.refused.noOutput')
  }
  if (next.opacity !== now.opacity && !capabilities.opacity) throw new Error('masks.refused.noOutput')
  if (!samePlacement(now, next)) {
    if (capabilities.transform !== 'movable') throw new Error('masks.refused.fixed')
    if (now.locked && next.locked) throw new Error('masks.refused.locked')
  }
  if (next.locked !== now.locked && !capabilities.lock) throw new Error('masks.refused.noLock')
  return next
}

/**
 * The untransformed box a layer was made in, in page percent. The pivot of
 * every move and turn is its centre plus the saved offset.
 *
 * @param {{bbox: Bbox, mask?: any}} region
 * @returns {Bbox}
 */
export function sourceBboxOf(region) {
  return region.mask?.sourceBbox ?? region.bbox
}

/**
 * The box a layer is drawn in - `LayerStyle::display_bounds`, in percent. The
 * rotation is done in pixels, because a percent of the width and a percent of
 * the height are not the same length.
 *
 * @param {Bbox} source
 * @param {Partial<LayerStyle>} layer
 * @param {{width?: number, height?: number}} page
 * @returns {Bbox}
 */
export function displayBbox(source, layer, page) {
  const style = sanitizeLayer(layer)
  const width = Number(page?.width) || 100
  const height = Number(page?.height) || 100
  const x = (source.x / 100) * width
  const y = (source.y / 100) * height
  const w = (source.w / 100) * width
  const h = (source.h / 100) * height
  const cx = x + w / 2
  const cy = y + h / 2
  const radians = (style.rotation * Math.PI) / 180
  const cos = Math.cos(radians)
  const sin = Math.sin(radians)
  const spanX = Math.abs(w * cos) + Math.abs(h * sin)
  const spanY = Math.abs(w * sin) + Math.abs(h * cos)
  const left = cx - spanX / 2 + style.offsetX
  const top = cy - spanY / 2 + style.offsetY
  return {
    x: (left / width) * 100,
    y: (top / height) * 100,
    w: (spanX / width) * 100,
    h: (spanY / height) * 100,
  }
}

/**
 * Where to draw a layer's own frame - the untransformed box moved by the
 * offset - and the angle to turn it by about its centre. That is the same
 * transform the compositor applies, so the frame sits on the pixels.
 *
 * @param {Bbox} source
 * @param {Partial<LayerStyle>} layer
 * @param {{width?: number, height?: number}} page
 * @returns {Bbox & {rotation: number}}
 */
export function layerFrame(source, layer, page) {
  const style = sanitizeLayer(layer)
  const width = Number(page?.width) || 100
  const height = Number(page?.height) || 100
  return {
    x: source.x + (style.offsetX / width) * 100,
    y: source.y + (style.offsetY / height) * 100,
    w: source.w,
    h: source.h,
    rotation: style.rotation,
  }
}

/**
 * Which longstrip positions a layer's boxes reach, by index into `heights`.
 *
 * A patch anchored on one page can reach across a join - drawn there, or
 * moved there - and the neighbour's tile then composites it too
 * (`tile::render`). Strip rows are the pages' heights stacked in order; the
 * boxes are page percent of the anchor, as a region's `bbox` is. A page one
 * pixel short of a box counts as reached: a tile refetched for nothing costs
 * a request, a stale one costs a wrong picture.
 *
 * @param {ReadonlyArray<number>} heights - page heights in pixels, in strip order
 * @param {number} anchor - the index the boxes are relative to
 * @param {ReadonlyArray<Bbox|null|undefined>} boxes
 * @returns {number[]} the reached indices, or every index when a height is unknown
 */
export function pagesReached(heights, anchor, boxes) {
  const every = heights.map((_, index) => index)
  if (!(anchor >= 0 && anchor < heights.length) || heights.some((height) => !(height > 0))) return every
  const offsets = []
  let top = 0
  for (const height of heights) {
    offsets.push(top)
    top += height
  }
  const reached = new Set()
  for (const box of boxes) {
    if (!box) continue
    const y0 = offsets[anchor] + (box.y / 100) * heights[anchor]
    const y1 = y0 + (box.h / 100) * heights[anchor]
    heights.forEach((height, index) => {
      if (offsets[index] < y1 + 1 && offsets[index] + height > y0 - 1) reached.add(index)
    })
  }
  return [...reached].sort((a, b) => a - b)
}
