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
 * The zooms `+` and `−` stop at. A ladder, not a fixed step: the range runs
 * from 40% to 12800%, and a step that is useful at 100% is no step at all at
 * 3200%. Whole numbers from 200% up, so the stops past it draw every image
 * pixel as a square of whole screen pixels, as an image editor's do.
 */
export const ZOOM_STOPS = Object.freeze([
  0.4, 0.5, 0.67, 0.8, 1, 1.25, 1.5, 2, 3, 4, 5, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128,
])

/**
 * The next stop up or down from `zoom`, inside `[min, max]`. A zoom between
 * two stops (a pinch left it there, or fit did) goes to the nearer stop in
 * the direction asked, never back to where it came from.
 *
 * @param {{zoom: number, direction: 'in'|'out', min: number, max: number}} spec
 * @returns {number}
 */
export function stepZoom(spec) {
  const zoom = finite(spec.zoom, 1)
  const slack = 0.005
  const next = spec.direction === 'out'
    ? [...ZOOM_STOPS].reverse().find((stop) => stop < zoom - slack) ?? spec.min
    : ZOOM_STOPS.find((stop) => stop > zoom + slack) ?? spec.max
  return Math.min(spec.max, Math.max(spec.min, next))
}

/**
 * Past this scale (drawn pixels per image pixel) the page and every raster
 * over it are drawn nearest-neighbour, so each image pixel is a crisp square.
 */
export const PIXELATED_FROM = 1

/** From this scale up a thin grid marks every image pixel, as in an image editor. */
export const PIXEL_GRID_FROM = 8

/**
 * How long, in CSS pixels, the canvas lets its column grow. Layout lengths in
 * both engines are fixed point and run out at about 33.5 million pixels;
 * half of that leaves room for the scroll room and the margins. A single
 * page never comes near it. A long webtoon chapter does, so its ceiling is
 * lower than `MAX_ZOOM`.
 */
export const MAX_DRAWN_EXTENT = 16_000_000

/**
 * The highest zoom at which a column `extent` page pixels long stays inside
 * `MAX_DRAWN_EXTENT`, capped at `max`.
 *
 * @param {{extent: number, max: number}} spec
 * @returns {number}
 */
export function zoomCeiling(spec) {
  const extent = finite(spec.extent, 0)
  if (!(extent > 0)) return spec.max
  return Math.min(spec.max, Math.floor((MAX_DRAWN_EXTENT / extent) * 100) / 100)
}

/** Below this length, in CSS pixels, a region is too small to find by eye. */
export const FOCUS_MIN_DRAWN = 40
/** A region that has to be zoomed to is drawn this long on its longer side. */
export const FOCUS_SMALL_TARGET = 160
/** Show on page never zooms past this by itself. */
export const FOCUS_MAX_AUTO = 4
/** A region drawn across more than this share of the view does not fit. */
export const FOCUS_FIT_SHARE = 0.9
/** And one zoomed out to fit takes this share. */
export const FOCUS_FILL_SHARE = 0.7

/**
 * The zoom Show on page should use for a region, or `null` to keep the
 * zoom on screen. The zoom is kept unless the region does not fit in the
 * view, or is drawn too small to see: then it is zoomed so it does, and no
 * further.
 *
 * @param {{zoom: number, regionWidth: number, regionHeight: number, contentWidth: number, contentHeight: number, min: number, max: number}} spec
 *   region sizes in page pixels, the view in CSS pixels
 * @returns {number|null}
 */
export function focusZoom(spec) {
  const zoom = finite(spec.zoom, 1)
  const regionWidth = Math.max(1, finite(spec.regionWidth, 1))
  const regionHeight = Math.max(1, finite(spec.regionHeight, 1))
  const viewWidth = finite(spec.contentWidth, 0)
  const viewHeight = finite(spec.contentHeight, 0)
  if (!(viewWidth > 0) || !(viewHeight > 0)) return null
  const fits = Math.min(
    (viewWidth * FOCUS_FILL_SHARE) / regionWidth,
    (viewHeight * FOCUS_FILL_SHARE) / regionHeight,
  )
  const clamp = (/** @type {number} */ value) => Math.min(spec.max, Math.max(spec.min, value))
  if (regionWidth * zoom > viewWidth * FOCUS_FIT_SHARE || regionHeight * zoom > viewHeight * FOCUS_FIT_SHARE) {
    // Never larger to make it fit: a fitted page under `min` would otherwise
    // be clamped up to the floor.
    const smaller = clamp(fits)
    return smaller < zoom ? smaller : null
  }
  if (Math.max(regionWidth, regionHeight) * zoom >= FOCUS_MIN_DRAWN) return null
  const seen = FOCUS_SMALL_TARGET / Math.max(regionWidth, regionHeight)
  return clamp(Math.max(zoom, Math.min(seen, fits, FOCUS_MAX_AUTO)))
}

/**
 * Where the scroller must be, on one axis, for the page to sit in the view:
 * centred when it fits, and when it does not, moved only as far as it takes
 * for no empty canvas to show at either end.
 *
 * @param {{start: number, size: number, viewStart: number, viewSize: number, scroll: number}} spec
 *   the page's and the view's extent on the axis, in client pixels
 * @returns {number}
 */
export function placeAxis(spec) {
  const { start, size, viewStart, viewSize, scroll } = spec
  if (size <= viewSize) return Math.max(0, scroll + (start + size / 2) - (viewStart + viewSize / 2))
  if (start > viewStart) return Math.max(0, scroll + (start - viewStart))
  const overshoot = viewStart + viewSize - (start + size)
  if (overshoot > 0) return Math.max(0, scroll - overshoot)
  return scroll
}

/**
 * Where the scroller must be, on one axis, for client position `point` to be
 * at the centre of the view.
 *
 * @param {{point: number, viewStart: number, viewSize: number, scroll: number}} spec
 * @returns {number}
 */
export function centreAxis(spec) {
  return Math.max(0, spec.scroll + spec.point - (spec.viewStart + spec.viewSize / 2))
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
