/**
 * The Pages list's arithmetic: what one row says, and which rows are worth
 * rendering. Pure - no Svelte, no DOM - because both are the kind of thing
 * that is either right or subtly wrong, and neither is testable through a
 * component.
 *
 * The list is a text list and it *is* the progress
 * indicator, so a row carries the status mark, the page label, the ratio of
 * regions finished to regions found, and the slim track that ratio fills.
 */

import { pageMark, pageCounts } from '../model/status.js'

/** Row height in px - the constraints table's page row. Rows do not wrap. */
export const ROW_HEIGHT = 25

/**
 * Below this many rows the list renders whole. A chapter runs to hundreds of
 * pages; twenty do not need a windowing scheme, and a list that is fully in
 * the DOM is a list whose focus, scroll anchoring and find-in-page all work
 * for free.
 */
export const VIRTUAL_THRESHOLD = 200

/** Rows kept mounted above and below the visible band. */
export const OVERSCAN = 6

/**
 * @typedef {Object} PageRow
 * @property {string} id
 * @property {number} index - 0-based position in the chapter
 * @property {string} labelKey - `pages.label.page`, or `.position` in longstrip
 * @property {string} number - the position, zero-padded: `01`
 * @property {import('../model/status.js').PageMark} mark
 * @property {number} cleaned - regions with a mask that needs no review
 * @property {number} total - regions of work found on the page (candidates apart)
 * @property {number} candidates - held candidates awaiting a choice
 * @property {number} percent - how full the track is, 0..100
 * @property {'warn'|'review'|'normal'} tone - the track's tone; never the only carrier of meaning
 * @property {string|null} skipReasonKey - i18n key, set only on a skipped page
 */

/**
 * One row of the Pages list.
 *
 * `cleaned` counts regions that are *finished*: masked and not flagged. A
 * region needing review is not one the user is done with, so it does not fill
 * the track - which is what makes the track and the mark agree.
 *
 * The three numbers come from `model/status.js#pageCounts` rather than from
 * `page.regions`, because **this row is drawn for every page in the chapter
 * and only three of them are resident**. Counting the list here
 * drew an empty track and a `0 / 0` ratio for every page outside the window.
 *
 * @param {import('../api/backend.js').ApiPage} page
 * @param {{ index: number, longstrip?: boolean }} options
 * @returns {PageRow}
 */
export function pageRow(page, options) {
  const { total, done: cleaned, review: flagged, candidates } = pageCounts(page)
  const percent = total > 0 ? Math.round((cleaned / total) * 100) : page.status === 'cleaned' ? 100 : 0

  return {
    id: page.id,
    index: options.index,
    labelKey: options.longstrip ? 'pages.label.position' : 'pages.label.page',
    number: padPosition(page.number ?? options.index + 1),
    mark: pageMark(page),
    cleaned,
    total,
    candidates,
    percent,
    tone: page.status === 'skipped' ? 'warn' : flagged > 0 ? 'review' : 'normal',
    skipReasonKey: page.status === 'skipped' ? (page.skipReason ?? null) : null,
  }
}

/**
 * `7` -> `07`. Two digits minimum, so a twenty-page chapter's labels line up;
 * longer chapters simply grow the column.
 *
 * @param {number} position
 * @returns {string}
 */
export function padPosition(position) {
  return String(position).padStart(2, '0')
}

/**
 * @typedef {Object} RowWindow
 * @property {boolean} virtual - false when every row is rendered
 * @property {number} start - first rendered index of the band
 * @property {number} end - one past the last rendered index of the band
 * @property {number|null} extra - one row rendered outside the band, or null
 * @property {boolean} extraBefore - whether `extra` renders above the band
 * @property {number} padTop - px of spacer above everything
 * @property {number} padGap - px of spacer between `extra` and the band
 * @property {number} padBottom - px of spacer below everything
 */

/**
 * Which rows to render for a scroller showing `viewportHeight` px of a list
 * `count` rows long, scrolled to `scrollTop`.
 *
 * `include` is the row that must stay mounted whatever the scroll position -
 * the focused one. Unmounting the focused element would drop focus to the
 * document, which during a run (when the list re-renders constantly) would
 * make the list unusable by keyboard.
 *
 * It is returned as `extra`, a single row rendered *outside* the band, rather
 * than by widening the band to reach it. Widening mounts every row in between:
 * a focused row 0 in a 400-row list scrolled to the end mounts all 400, which
 * is the exact pathology the virtualisation exists to avoid. The spacer above
 * is split around the extra row instead - `padTop`, the row, `padGap`, then
 * the band - so the total height is still `count * rowHeight` whichever side
 * the focused row is on.
 *
 * @param {{
 *   count: number,
 *   scrollTop?: number,
 *   viewportHeight?: number,
 *   include?: number,
 *   rowHeight?: number,
 *   overscan?: number,
 *   threshold?: number,
 * }} spec
 * @returns {RowWindow}
 */
export function rowWindow(spec) {
  const count = Number.isFinite(spec.count) ? Math.max(0, Math.trunc(spec.count)) : 0
  const threshold = spec.threshold ?? VIRTUAL_THRESHOLD
  if (count <= threshold) {
    return whole(count)
  }

  const rowHeight = Math.max(1, spec.rowHeight ?? ROW_HEIGHT)
  const overscan = Math.max(0, spec.overscan ?? OVERSCAN)
  const top = clampNumber(spec.scrollTop, 0, count * rowHeight)
  const height = clampNumber(spec.viewportHeight, 0, count * rowHeight)

  const start = Math.max(0, Math.floor(top / rowHeight) - overscan)
  const end = Math.min(count, Math.ceil((top + height) / rowHeight) + overscan + 1)

  // A non-integer `include` would break the arithmetic in both directions: the
  // spacers would be computed from 4.5 rows while `slice` truncated to 4.
  const include = Number.isFinite(spec.include) ? Math.trunc(/** @type {number} */ (spec.include)) : -1
  const inBand = include >= start && include < end
  const extra = include >= 0 && include < count && !inBand ? include : null

  if (extra === null) {
    return {
      virtual: true,
      start,
      end,
      extra: null,
      extraBefore: false,
      padTop: start * rowHeight,
      padGap: 0,
      padBottom: (count - end) * rowHeight,
    }
  }

  const before = extra < start
  return {
    virtual: true,
    start,
    end,
    extra,
    extraBefore: before,
    padTop: (before ? extra : start) * rowHeight,
    padGap: (before ? start - extra - 1 : extra - end) * rowHeight,
    padBottom: (count - (before ? end : extra + 1)) * rowHeight,
  }
}

/**
 * @param {number} count
 * @returns {RowWindow}
 */
function whole(count) {
  return {
    virtual: false,
    start: 0,
    end: count,
    extra: null,
    extraBefore: false,
    padTop: 0,
    padGap: 0,
    padBottom: 0,
  }
}

/**
 * @param {unknown} value
 * @param {number} min
 * @param {number} max
 * @returns {number}
 */
function clampNumber(value, min, max) {
  const number = Number(value)
  if (!Number.isFinite(number)) return min
  return Math.min(max, Math.max(min, number))
}
