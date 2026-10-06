/**
 * The canvas's zoom arithmetic. Pure - no Svelte, no DOM - because "fit
 * actually fits" and "the point under the cursor stays put" are both the kind
 * of thing that is either exactly right or subtly wrong, and neither is
 * testable through a component.
 *
 * The unit of zoom is **the page's own pixels**: `zoom` is the fraction of the
 * page's natural width the sheet is drawn at, so `1` is 1:1 and the bottom
 * pill's readout (`Math.round(zoom * 100)`) is the truth rather than a second
 * number kept in step by hand. Fit is the same scale, computed from the
 * viewport instead of chosen - which is why the canvas reports it (see
 * `reportFitScale`) and why one call, `displayZoom()`, answers "how big is the
 * page" for both cases.
 *
 * Bounds are *passed in*, never imported: `MIN_ZOOM` / `MAX_ZOOM` belong to
 * `state/editor.svelte.js`, and a pure module that reaches into rune-backed
 * state to read two constants is no longer pure.
 */

/** The design file's `NAT`, used when a page does not declare its own width. */
export const FALLBACK_NATURAL_WIDTH = 620

/** The design file's sheet aspect: `aspect-ratio: 2/3`, i.e. height/width. */
export const FALLBACK_RATIO = 1.5

/** Never draw a sheet narrower than this, whatever the viewport does. */
export const MIN_SHEET_WIDTH = 180

/**
 * The page at 1:1, in CSS pixels.
 *
 * @param {{width?: number}|null} page
 * @returns {number}
 */
export function naturalWidth(page) {
  const width = Number(page?.width)
  return Number.isFinite(width) && width > 0 ? width : FALLBACK_NATURAL_WIDTH
}

/**
 * height / width, for one page. Pages in a chapter are **not** assumed to
 * share it, since that assumption produced silent mis-sizing on webtoon
 * segments, which differ in height by construction (content-minima
 * splitting) and may differ in width too (the gutter). `strip.js#stripMetrics`
 * calls this once per page and
 * builds a per-page table from the results, rather than multiplying one
 * shared unit by a page count.
 *
 * @param {{width?: number, height?: number}|null} page
 * @returns {number}
 */
export function pageRatio(page) {
  const width = Number(page?.width)
  const height = Number(page?.height)
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    return FALLBACK_RATIO
  }
  return height / width
}

/**
 * The widest sheet that fits the viewport's *content box*.
 *
 * Single page: both dimensions, so the whole page is on screen - that is what
 * "fit" means. **Longstrip**: width only. A strip page is 800×4000; fitting it
 * vertically would draw it 150px wide, and a continuous column is scrolled
 * through by definition.
 *
 * **Fit never enlarges.** Passing `natural` caps the result at 1:1, which is
 * the conventional meaning of fit in an image viewer and the only sane answer
 * for a strip: a 800px-wide webtoon page stretched to a 1232px viewport is
 * 154% of a scan that has no more detail to give, and it slides under the
 * floating windows besides.
 *
 * @param {{contentWidth: number, contentHeight: number, ratio?: number, longstrip?: boolean, natural?: number}} spec
 * @returns {number} px
 */
export function fitWidth(spec) {
  const width = finite(spec.contentWidth, 0)
  const height = finite(spec.contentHeight, 0)
  const ratio = finite(spec.ratio, FALLBACK_RATIO) || FALLBACK_RATIO
  const byHeight = spec.longstrip ? Infinity : height / ratio
  const cap = spec.natural === undefined ? Infinity : finite(spec.natural, Infinity)
  return Math.max(MIN_SHEET_WIDTH, Math.min(width, byHeight, cap))
}

/**
 * The fit width expressed as a zoom - the number the readout would show if it
 * showed a number, and the number `zoomIn` steps up from.
 *
 * Deliberately **not** clamped to `MIN_ZOOM`: a 1600px page fitted into a
 * 1232px viewport is genuinely 32%, and reporting 40% because that is the
 * floor for a *chosen* zoom would make the readout lie. It never exceeds 1,
 * because `fitWidth` never enlarges.
 *
 * @param {{contentWidth: number, contentHeight: number, ratio?: number, longstrip?: boolean, natural: number}} spec
 * @returns {number}
 */
export function fitScale(spec) {
  const natural = finite(spec.natural, FALLBACK_NATURAL_WIDTH) || FALLBACK_NATURAL_WIDTH
  return round2(fitWidth({ ...spec, natural }) / natural)
}

/**
 * @param {number} value
 * @param {number} min
 * @param {number} max
 * @returns {number} `value` inside [min, max], rounded to two places
 */
export function clampZoom(value, min, max) {
  const number = Number(value)
  if (!Number.isFinite(number)) return min
  return round2(Math.min(max, Math.max(min, number)))
}

/**
 * The sheet's drawn width. One expression, used by the single page and by
 * every page of the strip, so nothing can disagree about how big a page is.
 *
 * @param {{natural: number, zoom: number}} spec
 * @returns {number} px
 */
export function sheetWidth(spec) {
  const natural = finite(spec.natural, FALLBACK_NATURAL_WIDTH) || FALLBACK_NATURAL_WIDTH
  const zoom = finite(spec.zoom, 1)
  return Math.max(MIN_SHEET_WIDTH, Math.round(natural * zoom))
}

/** How much one wheel notch moves the scale, and the ceiling on one event. */
export const WHEEL_SENSITIVITY = 0.01
export const WHEEL_MAX_STEP = 0.25

/**
 * What a pinch or a modifier-scroll should zoom to, or `null` for "do nothing".
 *
 * Multiplicative, so a pinch feels the same at 40% as at 240%, and bounded per
 * event so one flick of a mouse wheel cannot cross the whole range.
 *
 * The `null` case is the one worth stating: `zoom` can sit *below* `min` when
 * fit computed it, and pinching out from there would clamp *up* to the floor
 * and make the page bigger - the opposite of the gesture. (The bottom pill's
 * `−` is disabled in the same situation, for the same reason.)
 *
 * **Unrounded.** The canvas accumulates a gesture across events and frames,
 * and `setZoom` rounds what it keeps. Rounding each step here instead turned
 * every small step of a slow pinch back into the zoom it started from.
 *
 * @param {{deltaY: number, zoom: number, min: number, max: number}} spec
 * @returns {number|null}
 */
export function wheelZoom(spec) {
  const deltaY = finite(spec.deltaY, 0)
  const zoom = finite(spec.zoom, 1)
  if (deltaY > 0 && zoom <= spec.min) return null
  const factor = Math.min(
    1 + WHEEL_MAX_STEP,
    Math.max(1 - WHEEL_MAX_STEP, Math.exp(-deltaY * WHEEL_SENSITIVITY)),
  )
  const next = Math.min(spec.max, Math.max(spec.min, zoom * factor))
  return next === zoom ? null : next
}

/**
 * @typedef {Object} Box
 * @property {number} left
 * @property {number} top
 * @property {number} width
 * @property {number} height
 */

/**
 * Where the scroller must be so that the point under the pointer stays under
 * the pointer across a scale change.
 *
 * Taking the content's box *before* and *after* rather than deriving the new
 * geometry is what makes this correct without knowing anything about the
 * scroller's padding or its `justify-content: center` - both of which move the
 * content in ways a scale factor alone cannot predict.
 *
 * @param {{pointerX: number, pointerY: number, before: Box, after: Box, scrollLeft: number, scrollTop: number}} spec
 * @returns {{left: number, top: number}}
 */
export function anchoredScroll(spec) {
  const { before, after } = spec
  const rx = before.width > 0 ? (spec.pointerX - before.left) / before.width : 0
  const ry = before.height > 0 ? (spec.pointerY - before.top) / before.height : 0
  return {
    left: Math.max(0, spec.scrollLeft + (after.left + rx * after.width - spec.pointerX)),
    top: Math.max(0, spec.scrollTop + (after.top + ry * after.height - spec.pointerY)),
  }
}

/**
 * @param {unknown} value
 * @param {number} fallback
 * @returns {number}
 */
function finite(value, fallback) {
  const number = Number(value)
  return Number.isFinite(number) ? number : fallback
}

/**
 * `1 + 0.15 + 0.15` is otherwise `1.2999999999999998`, which then accumulates
 * through the autosave record and back (the reason `setZoom` already rounds).
 *
 * @param {number} value
 * @returns {number}
 */
function round2(value) {
  return Math.round(value * 100) / 100
}
