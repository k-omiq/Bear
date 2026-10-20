/**
 * The longstrip column's arithmetic: how tall it is, which of its pages are
 * worth mounting, which ones are on screen, and which one the reader is
 * actually looking at.
 *
 * Pure - no Svelte, no DOM. Nothing in the fixtures is long enough to exercise
 * the virtualisation threshold in a browser, so the tests are the proof.
 *
 * **Every page is measured, none is assumed.** Revision 1 of this module took
 * one `unit` - page 0's height plus the gap - and multiplied it, the same trade
 * `pagerows.js#rowWindow` makes for the Pages list. That trade is sound there,
 * where a row is 25px of text by construction, and it was never sound here: it
 * was true of the mock's fixtures (800×4000 throughout) and of nothing else.
 * A webtoon chapter's segments are not uniform **by construction** - the strip
 * splits at content minima inside a ±500px tolerance band, so
 * consecutive segments differ by hundreds of pixels - and one unit standing for
 * all of them mis-sized the column *silently*: spacers, the position readout
 * and `goToPage` all drifting together, with a dev-mode `console.warn` as the
 * only sign.
 *
 * So the geometry is a table, not a multiplication. [`stripMetrics`] builds it
 * once per (page list, scale) and everything else reads it:
 *
 *     widths[i]    the sheet's drawn width, from page i's own width
 *     units[i]     page i's drawn height plus the gap under it
 *     offsets[i]   the top of page i, offsets[count] being the whole column
 *
 * **Geometry.** CanvasStage gives the column its full width and height and
 * positions each mounted sheet at `offsets[index]`. Virtualisation cannot
 * change its size, and fractional sheet heights cannot accumulate spacer
 * rounding. A focused sheet outside the band uses its own offset too.
 *
 * **Position.** `columnTop` is the column's top edge relative to the viewport.
 * The canvas measures its origin on resize, then subtracts `scrollTop` during
 * scrolling, keeping viewport padding out of the window arithmetic.
 */

import { naturalWidth, pageRatio, sheetWidth } from './zoom.js'
import { proxyPlan } from '../api/tile.js'

/** Longstrip pages meet on the same row boundary as the source pixels. */
export const STRIP_GAP = 0

/** Below this many pages the column renders whole. */
export const VIRTUAL_THRESHOLD = 12

/** Pages kept mounted above and below the preloaded band. */
export const OVERSCAN = 1

/**
 * How far past the viewport, in viewport heights, pages and their tiles are
 * mounted - and therefore fetched - before they scroll into view.
 *
 * The canvas does this itself rather than leaving it to `loading="lazy"`:
 * WebKit, which is the shipped webview on macOS, starts a lazy image only when
 * it enters the *scroller's* visible box, so in this nested scroller every
 * page arrived blank and drew in as it was read.
 */
export const PRELOAD_SCREENS = 1

/**
 * Pages kept warm on each side of the page at the viewport's centre: mounted,
 * with every tile of both variants, and with their regions resident. A
 * scroll or a zoom inside this range never shows a page that has to load
 * again. Everything past it is let go: its sheet leaves the DOM, which hands
 * the decoded tiles back, and its regions are evicted.
 */
export const KEEP_PAGES = 5

/**
 * The most decoded tile memory the warm range may hold, in bytes. A chapter of
 * 690×1600 segments keeps all five pages a side at about 100 MB; a chapter of
 * raw 800×20000 segments, whose proxies are 64 MB a variant, keeps none whole
 * and loads each by its tile window, as before ([`keepRange`]).
 */
export const KEEP_BUDGET_BYTES = 192 * 1024 * 1024

/**
 * What one page's tiles cost decoded, both variants: the proxy's pixels at
 * four bytes each, twice.
 *
 * @param {{width?: number, height?: number}|null|undefined} page
 * @returns {number} bytes
 */
export function pageBytes(page) {
  const plan = proxyPlan(page)
  return plan.width * plan.height * 4 * 2
}

/**
 * The warm range around `centre`: grown one page at a time, alternating below
 * and above, until `keep` pages sit on each side or the next page would take
 * the range past `budget`. No page may take more than its even share of the
 * budget, `budget / (2 * keep + 1)`: a page that would (a raw 800×20000
 * segment is 128 MB of tiles) ends the range on its side, and a centre page
 * that would leaves the range empty. Such a page is left to the viewport band,
 * which mounts it anyway, and to its own tile window, which loads only what is
 * near the viewport.
 *
 * @param {{count: number, centre: number, keep?: number, budget?: number, cost: (index: number) => number}} spec
 * @returns {{start: number, end: number}} `end` is one past the last index
 */
export function keepRange(spec) {
  const count = Math.max(0, Math.trunc(finite(spec.count, 0)))
  if (count === 0) return { start: 0, end: 0 }
  const keep = Math.max(0, Math.trunc(finite(spec.keep, KEEP_PAGES)))
  const budget = Math.max(0, finite(spec.budget, KEEP_BUDGET_BYTES))
  const centre = clamp(Math.trunc(finite(spec.centre, 0)), 0, count - 1)

  let start = centre
  let end = centre + 1
  const share = budget / (2 * keep + 1)
  let spent = Math.max(0, finite(spec.cost(centre), 0))
  if (spent > share) return { start: centre, end: centre }
  let below = true
  let above = true
  for (let step = 1; step <= keep && (below || above); step += 1) {
    for (const index of [centre + step, centre - step]) {
      const side = index > centre ? below : above
      if (!side || index < 0 || index >= count) continue
      const cost = Math.max(0, finite(spec.cost(index), 0))
      if (cost > share || spent + cost > budget) {
        if (index > centre) below = false
        else above = false
        continue
      }
      spent += cost
      if (index > centre) end = index + 1
      else start = index
    }
  }
  return { start, end }
}

/**
 * Where the column's top must sit, relative to the viewport, so that the
 * content under `anchorY` (px below the viewport's top edge) stays under it
 * when the column's geometry changes from `before` to `after` - a zoom.
 *
 * The anchor is held as a page and a fraction of that page rather than as a
 * fraction of the whole column, because sheet widths are rounded to whole
 * pixels and the column does not scale exactly with the zoom.
 *
 * @param {{before: StripMetrics, after: StripMetrics, columnTop: number, anchorY: number}} spec
 * @returns {number} the new `columnTop`
 */
export function anchorColumnTop(spec) {
  const { before, after } = spec
  const anchorY = finite(spec.anchorY, 0)
  const columnTop = finite(spec.columnTop, 0)
  if (before.count === 0 || after.count !== before.count) return columnTop
  const y = clamp(anchorY - columnTop, 0, before.total)
  const index = indexAt(before, y)
  const fraction = before.units[index] > 0 ? (y - before.offsets[index]) / before.units[index] : 0
  return anchorY - (after.offsets[index] + fraction * after.units[index])
}

/**
 * One page's share of the column: its height plus the gap under it.
 *
 * @param {{pageHeight: number, gap?: number}} spec
 * @returns {number} px
 */
export function stripUnit(spec) {
  const height = Math.max(1, finite(spec.pageHeight, 1))
  return height + Math.max(0, finite(spec.gap, STRIP_GAP))
}

/**
 * @typedef {Object} StripMetrics
 * @property {number} count - pages in the column
 * @property {number[]} widths - each sheet's drawn width, px
 * @property {number[]} units - each page's drawn height plus the gap, px
 * @property {number[]} offsets - `count + 1` cumulative tops; the last is `total`
 * @property {number} total - the whole column's height, px
 */

/**
 * The column's geometry at one scale: what every page is drawn at, and where
 * every page starts.
 *
 * `scale` is the fraction of a page's own pixels the sheet is drawn at - the
 * same unit `zoom.js` uses, and the same number the bottom pill reports - so a
 * page's drawn width is its own width times the scale and never the column's.
 * Two pages of different widths therefore stay different widths, which is what
 * the gutter means on screen: a 700px page beside an 800px one is
 * narrower, not stretched.
 *
 * `sheetWidth` and `pageRatio` are imported rather than re-derived so that the
 * arithmetic here and the sheet `CanvasStage.svelte` actually draws come from
 * one expression. `widths[i]` is passed straight to `PageSheet`, so the two
 * cannot disagree about how big a page is.
 *
 * @param {{pages?: Array<{width?: number, height?: number}>, scale?: number, gap?: number}} spec
 * @returns {StripMetrics}
 */
export function stripMetrics(spec) {
  const gap = Math.max(0, finite(spec.gap, STRIP_GAP))
  const scale = finite(spec.scale, 1)
  const pages = Array.isArray(spec.pages) ? spec.pages : []

  /** @type {number[]} */ const widths = []
  /** @type {number[]} */ const units = []
  /** @type {number[]} */ const offsets = [0]

  for (const page of pages) {
    const width = sheetWidth({ natural: naturalWidth(page), zoom: scale })
    const unit = stripUnit({ pageHeight: width * pageRatio(page), gap })
    widths.push(width)
    units.push(unit)
    offsets.push(offsets[offsets.length - 1] + unit)
  }

  return { count: pages.length, widths, units, offsets, total: offsets[offsets.length - 1] }
}

/**
 * The page whose band contains `y`, clamped to the column. Binary search
 * rather than a division, because the offsets are no longer a multiple of
 * anything.
 *
 * @param {StripMetrics} metrics
 * @param {number} y
 * @returns {number}
 */
function indexAt(metrics, y) {
  if (metrics.count <= 0) return 0
  let low = 0
  let high = metrics.count - 1
  while (low < high) {
    const mid = (low + high + 1) >> 1
    if (metrics.offsets[mid] <= y) low = mid
    else high = mid - 1
  }
  return low
}

/**
 * @typedef {Object} StripWindow
 * @property {boolean} virtual - false when every page is mounted
 * @property {number} start - first mounted index
 * @property {number} end - one past the last mounted index
 * @property {number} padTop - px of spacer above
 * @property {number} padBottom - px of spacer below
 * @property {number|null} extra - focused page outside the band, positioned at its own offset
 */

/**
 * Which pages to mount for a viewport `viewportHeight` px tall whose top edge
 * sits `-columnTop` px into the column `metrics` describes.
 *
 * `margin` extends the viewport by that many px at both ends before the band
 * is taken, so the pages about to scroll in are already mounted; `overscan`
 * then adds whole pages beyond it.
 *
 * `include` is the page that must stay mounted whatever the scroll position -
 * the one holding focus. Unmounting the focused element drops focus to the
 * document. It is returned separately so a distant focus never widens the band.
 *
 * `keep` is the warm range ([`keepRange`]) and `hold` the band a zoom gesture
 * started from; both are added to the band, so neither can unmount a page the
 * viewport still wants, and a gesture cannot unmount the pages it began on.
 *
 * @param {{
 *   metrics?: StripMetrics,
 *   columnTop?: number,
 *   viewportHeight?: number,
 *   margin?: number,
 *   include?: number,
 *   overscan?: number,
 *   threshold?: number,
 *   keep?: {start: number, end: number}|null,
 *   hold?: {start: number, end: number}|null,
 * }} spec
 * @returns {StripWindow}
 */
export function stripWindow(spec) {
  const metrics = spec.metrics ?? EMPTY
  const count = metrics.count
  const threshold = finite(spec.threshold, VIRTUAL_THRESHOLD)
  if (count <= threshold) return { virtual: false, start: 0, end: count, padTop: 0, padBottom: 0, extra: null }

  const overscan = Math.max(0, Math.trunc(finite(spec.overscan, OVERSCAN)))
  const margin = Math.max(0, finite(spec.margin, 0))
  const top = clamp(-finite(spec.columnTop, 0) - margin, 0, metrics.total)
  const bottom = clamp(-finite(spec.columnTop, 0) + finite(spec.viewportHeight, 0) + margin, top, metrics.total)

  let start = Math.max(0, indexAt(metrics, top) - overscan)
  let end = Math.min(count, indexAt(metrics, bottom) + 1 + overscan)
  // The warm range and a live gesture's starting band are unions, never
  // limits: what they name stays mounted wherever the viewport has got to.
  for (const range of [spec.keep, spec.hold]) {
    if (!range || !(range.end > range.start)) continue
    start = Math.max(0, Math.min(start, Math.trunc(range.start)))
    end = Math.min(count, Math.max(end, Math.trunc(range.end)))
  }

  const include = spec.include
  const extra = Number.isInteger(include) && include >= 0 && include < count &&
    (include < start || include >= end) ? include : null

  return {
    virtual: true,
    start,
    end,
    padTop: metrics.offsets[start],
    padBottom: metrics.total - metrics.offsets[end],
    extra,
  }
}

/**
 * The positions actually on screen, in strip order - what `scopePageIndices()`
 * answers for a longstrip chapter, and therefore what the Layers panel scopes
 * itself to.
 *
 * A page counts as on screen when any part of it overlaps the viewport, so a
 * viewport that stops exactly on the next page's first row does not include
 * it. The result is never empty for a chapter that has pages: a viewport
 * shorter than one page still has exactly one page in it.
 *
 * @param {{metrics?: StripMetrics, columnTop?: number, viewportHeight?: number}} spec
 * @returns {number[]}
 */
export function visibleIndices(spec) {
  const metrics = spec.metrics ?? EMPTY
  if (metrics.count === 0) return []
  const top = -finite(spec.columnTop, 0)
  const bottom = top + Math.max(0, finite(spec.viewportHeight, 0))

  const first = indexAt(metrics, top)
  let last = indexAt(metrics, bottom)
  if (last > first && metrics.offsets[last] >= bottom) last -= 1

  const indices = []
  for (let index = first; index <= last; index += 1) indices.push(index)
  return indices
}

/**
 * Which page occupies the centre of the viewport - the strip's answer to
 * "which page am I on", and what the bottom pill's readout follows.
 *
 * @param {{metrics?: StripMetrics, columnTop?: number, viewportHeight?: number}} spec
 * @returns {number} a page index, or 0 for an empty chapter
 */
export function centreIndex(spec) {
  const metrics = spec.metrics ?? EMPTY
  if (metrics.count === 0) return 0
  const centre = -finite(spec.columnTop, 0) + finite(spec.viewportHeight, 0) / 2
  return indexAt(metrics, centre)
}

/**
 * How far to scroll so that page `index` sits at the top of the viewport -
 * the "scroll the strip to this position" half of `goToPage`.
 *
 * `index === count` is a position too: the bottom of the column.
 *
 * @param {{index: number, metrics?: StripMetrics, scrollTop?: number, columnTop?: number}} spec
 * @returns {number} the scroller's new `scrollTop`
 */
export function scrollTopFor(spec) {
  const metrics = spec.metrics ?? EMPTY
  const index = clamp(Math.trunc(finite(spec.index, 0)), 0, metrics.count)
  // Where the column's top sits in the scroller's own coordinates: its current
  // offset from the viewport plus however far the scroller is already down.
  const columnOrigin = finite(spec.columnTop, 0) + finite(spec.scrollTop, 0)
  return Math.max(0, Math.round(columnOrigin + metrics.offsets[index]))
}

/** The column of a chapter with no pages. Shared, and never mutated. */
const EMPTY = /** @type {StripMetrics} */ (
  Object.freeze({ count: 0, widths: [], units: [], offsets: [0], total: 0 })
)

/**
 * @param {number} value
 * @param {number} min
 * @param {number} max
 * @returns {number}
 */
function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value))
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
