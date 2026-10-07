/**
 * The stroke planner, in JavaScript: a polyline and a brush, to a list of dabs.
 *
 * **A port of `crates/cleaner-core/src/paint/plan.rs`, line for line.** It is
 * the frontend half of the bargain - *parity of the plan, not of the
 * pixels*. The same polyline planned here and planned in Rust must produce
 * the same dab list, because the preview draws
 * that list scaled to the proxy while the commit stamps it at native
 * resolution. Only the sampling grid is allowed to differ.
 *
 * So this file follows the Rust one exactly and deliberately: the same clamps,
 * the same `MIN_RADIUS` floor, the same arc-length walk with the leftover
 * carried **across** segment joins, and the same "a tap is a mark" rule for a
 * one-point stroke. Nothing here reads a clock or a random number generator.
 * `seed` is carried by the seam and consumed by neither side in Phase 1.
 *
 * Two things Rust does not have to do live here as well, because the pointer
 * speaks a different coordinate space than the page does:
 *
 *   - [`nativePoints`] converts the draft's **page percent** to native page
 *     pixels, which is the only space the planner works in.
 *   - [`brushFromParams`] reads the tool parameter object the editor already
 *     holds and produces the `BrushSpec` shape, with the same defaults
 *     `src/lib/editor/paint.js` sends across the seam.
 */

/**
 * @typedef {Object} StrokePoint
 * @property {number} x - native page pixels
 * @property {number} y - native page pixels
 * @property {number} p - pressure, `0..1`; `0.5` where the device did not say
 */

/**
 * @typedef {Object} BrushSpec
 * @property {number} size - diameter, in native page pixels
 * @property {number} hardness - `0..100`
 * @property {number} flow - `0..100`, per-dab alpha before opacity caps it
 * @property {number} opacity - `0..100`, the stroke-level ceiling
 * @property {number} spacing - percent **of the diameter** between dabs, `1..50`
 * @property {boolean} pressureSize
 * @property {boolean} pressureOpacity
 * @property {number} [seed]
 */

/**
 * @typedef {Object} PlannedDab
 * @property {number} x - native page pixels
 * @property {number} y - native page pixels
 * @property {number} radius - native page pixels
 * @property {number} alpha - `0..1`
 */

/**
 * The smallest dab that is still a dab, in native page pixels. Matches
 * `plan.rs#MIN_RADIUS`.
 */
export const MIN_RADIUS = 0.5

/** Matches `plan.rs#MIN_SPACING_PERCENT` / `MAX_SPACING_PERCENT`. */
export const MIN_SPACING_PERCENT = 1
export const MAX_SPACING_PERCENT = 50

/**
 * `plan.rs#clamp`, including its treatment of a non-finite value as the floor
 * - which is what makes a `NaN` spacing the *densest* legal brush rather than
 * an infinite loop.
 *
 * @param {number} value
 * @param {number} lo
 * @param {number} hi
 * @returns {number}
 */
function clamp(value, lo, hi) {
  if (!Number.isFinite(value)) return lo
  return Math.min(Math.max(value, lo), hi)
}

/**
 * The distance between two dab centres, in native page pixels.
 *
 * @param {BrushSpec} brush
 * @returns {number}
 */
export function spacingPx(brush) {
  const percent = clamp(Number(brush?.spacing), MIN_SPACING_PERCENT, MAX_SPACING_PERCENT)
  const size = Number.isFinite(Number(brush?.size)) ? Math.max(Number(brush.size), 1) : 1
  return Math.max((percent / 100) * size, 0.1)
}

/**
 * @param {BrushSpec} brush
 * @param {number} pressure
 * @returns {number}
 */
export function radiusAt(brush, pressure) {
  const scale = brush?.pressureSize ? clamp(Number(pressure), 0, 1) : 1
  const size = Number.isFinite(Number(brush?.size)) ? Math.max(Number(brush.size), 0) : 0
  return Math.max((size / 2) * scale, MIN_RADIUS)
}

/**
 * @param {BrushSpec} brush
 * @param {number} pressure
 * @returns {number}
 */
export function alphaAt(brush, pressure) {
  const flow = clamp(Number(brush?.flow), 0, 100) / 100
  const scale = brush?.pressureOpacity ? clamp(Number(pressure), 0, 1) : 1
  return clamp(flow * scale, 0, 1)
}

/**
 * @param {BrushSpec} brush
 * @param {number} x
 * @param {number} y
 * @param {number} pressure
 * @returns {PlannedDab}
 */
function dabAt(brush, x, y, pressure) {
  return { x, y, radius: radiusAt(brush, pressure), alpha: alphaAt(brush, pressure) }
}

/**
 * Walk the polyline and emit the dabs, in native page pixels.
 *
 * A single point is one dab: a tap is a mark, not nothing. Two coincident
 * points are still one dab, because the walk advances by arc length and a
 * zero-length segment has none to give.
 *
 * **The plan of a prefix is a prefix of the plan.** The walk is sequential and
 * its only carried state is the leftover arc length, so appending a point to
 * the polyline can only append dabs - never move one already emitted. That is
 * what lets `PaintLayer` draw a live stroke incrementally instead of
 * re-stamping every dab on every frame.
 *
 * @param {Array<{x: number, y: number, p?: number}>} points
 * @param {BrushSpec} brush
 * @returns {PlannedDab[]}
 */
export function planStroke(points, brush) {
  /** @type {PlannedDab[]} */
  const dabs = []
  const path = (points ?? []).filter(
    (point) => point && Number.isFinite(point.x) && Number.isFinite(point.y),
  )
  const first = path[0]
  if (!first) return dabs

  dabs.push(dabAt(brush, first.x, first.y, pressureOf(first)))
  if (path.length === 1) return dabs

  const step = spacingPx(brush)
  // How far past the last emitted dab the walk has travelled. A segment
  // shorter than what is left over contributes its length and no dab, which is
  // what keeps spacing uniform *across* segment joins rather than restarting at
  // every pointer sample.
  let carried = 0

  for (let index = 0; index + 1 < path.length; index += 1) {
    const a = path[index]
    const b = path[index + 1]
    const dx = b.x - a.x
    const dy = b.y - a.y
    const length = Math.hypot(dx, dy)
    if (!Number.isFinite(length) || length <= 0) continue

    const ap = pressureOf(a)
    const bp = pressureOf(b)
    let travelled = step - carried
    while (travelled <= length) {
      const t = travelled / length
      dabs.push(dabAt(brush, a.x + dx * t, a.y + dy * t, ap + (bp - ap) * t))
      travelled += step
    }
    carried = length - (travelled - step)
  }

  return dabs
}

/**
 * The bounding box of a planned stroke, grown by the largest dab's reach.
 * Mirrors `plan.rs#bounds_of`.
 *
 * @param {PlannedDab[]} dabs
 * @param {number} [margin]
 * @returns {{x0: number, y0: number, x1: number, y1: number}|null}
 */
export function boundsOfDabs(dabs, margin = 0) {
  if (!dabs || dabs.length === 0) return null
  let x0 = Infinity
  let y0 = Infinity
  let x1 = -Infinity
  let y1 = -Infinity
  for (const dab of dabs) {
    const reach = dab.radius + margin
    x0 = Math.min(x0, dab.x - reach)
    y0 = Math.min(y0, dab.y - reach)
    x1 = Math.max(x1, dab.x + reach)
    y1 = Math.max(y1, dab.y + reach)
  }
  return { x0, y0, x1, y1 }
}

/**
 * A point's pressure **as the planner reads it**, which is verbatim.
 *
 * Rust's `StrokePoint.p` is an `f64` with no absent case, so `plan_stroke`
 * takes whatever it is given - a genuine zero included, which is how a
 * pressure-size brush arrives at `MIN_RADIUS`. Only the missing case is this
 * side's to invent, and it invents the same `0.5` the seam does.
 *
 * The *seam's* stricter rule - `p > 0 ? p : 0.5`, which folds a reported zero
 * up to a half - belongs to [`nativePoints`] and to `paint.js#paintPointsOf`,
 * one step earlier. Putting it here too would mean the preview could never draw
 * the smallest dab the commit can produce.
 *
 * @param {{p?: number}} point
 * @returns {number}
 */
function pressureOf(point) {
  const p = Number(point?.p)
  return Number.isFinite(p) ? p : 0.5
}

/**
 * The seam's pressure convention: `0.5` where the device did not say, and a
 * reported zero counts as not saying (`paint.js#paintPointsOf`, and
 * 00-design.md risk 4).
 *
 * @param {{p?: number}} point
 * @returns {number}
 */
function seamPressureOf(point) {
  const p = Number(point?.p)
  return Number.isFinite(p) && p > 0 ? p : 0.5
}

/**
 * Draft points (**page percent**, which is what every gesture in
 * `gesture.js` works in) to **native page pixels**, which is the only space
 * the planner understands.
 *
 * The page's two axes are scaled by different amounts - a sheet is drawn at
 * the page's aspect ratio - so this is two multiplications and not one. It is
 * exactly the conversion `DraftPreview.svelte` does for its swept capsule, and
 * `paint.js` does not do it at all: the seam carries percent and Rust converts
 * on its own side.
 *
 * @param {Array<{x: number, y: number, p?: number}>} points - page percent
 * @param {number} pageWidth - native page pixels
 * @param {number} pageHeight - native page pixels
 * @returns {StrokePoint[]}
 */
export function nativePoints(points, pageWidth, pageHeight) {
  const w = Number(pageWidth) > 0 ? Number(pageWidth) : 1
  const h = Number(pageHeight) > 0 ? Number(pageHeight) : 1
  return (points ?? [])
    .filter((point) => point && Number.isFinite(point.x) && Number.isFinite(point.y))
    .map((point) => ({
      x: (point.x / 100) * w,
      y: (point.y / 100) * h,
      p: seamPressureOf(point),
    }))
}

/**
 * The `BrushSpec` a tool's parameter object describes.
 *
 * The defaults are `src/lib/editor/paint.js#paintParamsOf`'s, so the preview
 * plans the stroke the seam will carry rather than a similar one. The clone
 * stamp carries the same rows, and `pressureSize` / `pressureOpacity` are hardcoded on both sides
 * in Phase 1.
 *
 * @param {Record<string, unknown>} params
 * @param {{size?: number}} [fallback]
 * @returns {BrushSpec}
 */
export function brushFromParams(params = {}, fallback = {}) {
  const p = /** @type {Record<string, any>} */ (params ?? {})
  return {
    size: Math.max(1, Number(p.size ?? fallback.size ?? 28)),
    hardness: clamp(Number(p.hardness ?? 70), 0, 100),
    flow: clamp(Number(p.flow ?? 100), 0, 100),
    opacity: clamp(Number(p.opacity ?? 100), 0, 100),
    spacing: clamp(Number(p.spacing ?? 12), MIN_SPACING_PERCENT, MAX_SPACING_PERCENT),
    pressureSize: true,
    pressureOpacity: false,
    seed: 0,
  }
}
