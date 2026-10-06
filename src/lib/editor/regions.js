/**
 * What the canvas needs to know about a region before it draws one: the order
 * the keyboard walks them in, and what each one is called.
 *
 * Pure, and it invents no vocabulary. Everything a region *says* comes from
 * `maskrows.js#maskRow` - the same glyph, the same status word, the same engine
 * name and the same review reason the Layers panel shows for that region - so
 * the canvas and the panel can never describe one region two ways. This module
 * only decides which of those facts go into a one-sentence accessible name.
 */

import { isDetected } from '../model/masks.js'
import { REVIEW_GLYPH, maskRow } from './maskrows.js'

/**
 * Rows are banded in tenths of the page's height before being ordered across.
 * Quantising rather than comparing `y` with a tolerance is deliberate: a
 * tolerance comparison is not transitive, and `Array#sort` on a non-transitive
 * comparator gives a different answer depending on the input order.
 */
export const BAND = 10

/**
 * @typedef {Object} RegionMarker
 * @property {string} id
 * @property {{x: number, y: number, w: number, h: number}} bbox - percentages of the page
 * @property {'applied'|'review'|'declined'|'unexamined'|'detected'|'candidate'} status
 * @property {boolean} masked - whether a mask is applied, i.e. whether the tint has anything to tint
 * @property {string|null} badge - the persistent glyph, or null when the region carries no marker
 * @property {string} nameKey
 * @property {Object} nameParams
 */

/**
 * One region's marker: what to draw on it, and what to call it.
 *
 * A declined region always carries its badge - it is the one outcome where
 * nothing visibly happened and the user would otherwise never find it.
 * A flagged region's badge is conditional, because the
 * page would otherwise be covered in triangles the moment a chapter finishes.
 * A candidate carries no badge: its dotted outline is always drawn instead
 * (`RegionLayer`), which is enough to find it without adding to the marks.
 *
 * @param {import('../api/backend.js').ApiRegion} region
 * @param {{ marksVisible?: boolean }} [options] - whether the mask overlay or the review filter is on
 * @returns {RegionMarker}
 */
export function regionMarker(region, options = {}) {
  const row = maskRow(region, false)
  const declined = row.status === 'declined'
  // A detection flagged for repair keeps its detected status, mask and
  // name, so its flag is read from the reason it carries.
  const detection = isDetected(region)
  const flagged = row.status === 'review' || (detection && !!row.reasonKey)

  return {
    id: region.id,
    bbox: region.bbox,
    status: row.status,
    // A detection's mask is the cleaner's input, not a cleaned layer, flagged
    // or not.
    masked: !!region.mask && !detection,
    badge: declined ? row.glyph : flagged && options.marksVisible ? REVIEW_GLYPH : null,
    nameKey: row.reasonKey ? 'canvas.region.nameFlagged' : 'canvas.region.name',
    nameParams: row.reasonKey
      ? { statusKey: row.statusKey, titleKey: row.titleKey, reasonKey: row.reasonKey }
      : { statusKey: row.statusKey, titleKey: row.titleKey },
  }
}

/**
 * The regions of a page in reading order: down the page in bands, and across
 * each band in the project's reading direction. Right-to-left is the default
 * for manga, and the arrow keys on the canvas have
 * to agree with the order the page is actually read in or they are a puzzle.
 *
 * The input array is not mutated - it is `editor`'s own rune-backed array.
 *
 * @param {import('../api/backend.js').ApiRegion[]} regions
 * @param {'rtl'|'ltr'} direction
 * @returns {import('../api/backend.js').ApiRegion[]}
 */
export function readingOrder(regions, direction) {
  const sign = direction === 'ltr' ? 1 : -1
  return regions
    .map((region, index) => ({ region, index }))
    .sort((a, b) => {
      const bandA = Math.floor((a.region.bbox?.y ?? 0) / BAND)
      const bandB = Math.floor((b.region.bbox?.y ?? 0) / BAND)
      if (bandA !== bandB) return bandA - bandB
      const across = sign * ((a.region.bbox?.x ?? 0) - (b.region.bbox?.x ?? 0))
      // Ties fall back to the detector's own order so the result is stable.
      return across !== 0 ? across : a.index - b.index
    })
    .map((entry) => entry.region)
}
