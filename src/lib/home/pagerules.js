/**
 * The Pages screen's arithmetic: which page edits the backend will take, and
 * what a removal costs. Pure, so each answer is testable without a component.
 *
 * The rule is `src-tauri/src/library.rs#check_page_order`, said again here so
 * the screen can disable a button and say why before the user presses it. The
 * backend still checks; a refusal there comes back as a notice.
 *
 * **A page with work keeps its place.** Region ids are built on page positions
 * (`c5-p003-r0`), so a page holding masks, cleans or edits cannot move, and no
 * page before it can be removed or inserted. A longstrip with work only takes
 * pages at its end. A chapter keeps at least one page.
 */

/**
 * @typedef {import('../api/backend.js').ChapterPages} ChapterPages
 * @typedef {{key: string, params: Record<string, unknown>}} Refusal
 */

/**
 * Why `next` is refused, or null. `next` lists the current positions in their
 * new order, with `-1` for a new page; `dropped` are positions leaving.
 *
 * @param {ChapterPages} listing
 * @param {number[]} next
 * @param {number[]} [dropped]
 * @returns {Refusal|null}
 */
export function orderRefusal(listing, next, dropped = []) {
  if (listing.busy) return { key: 'home.pagesScreen.why.busy', params: {} }
  if (next.length === 0) return { key: 'home.pagesScreen.why.lastPage', params: {} }
  const work = listing.worked
  const count = listing.pages.length
  const append = next.length >= count && listing.pages.every((_, i) => next[i] === i)
  if (listing.longstrip && !append && work.some(Boolean)) {
    return { key: 'home.pagesScreen.why.longstrip', params: {} }
  }
  for (let at = 0; at < count; at += 1) {
    if (work[at] && !dropped.includes(at) && next[at] !== at) {
      return { key: 'home.pagesScreen.why.pinned', params: { page: at + 1 } }
    }
  }
  return null
}

/** @param {number} count */
function identity(count) {
  return Array.from({ length: count }, (_, i) => i)
}

/**
 * @param {ChapterPages} listing
 * @param {number[]} positions
 */
export function removeRefusal(listing, positions) {
  const next = identity(listing.pages.length).filter((i) => !positions.includes(i))
  return orderRefusal(listing, next, positions)
}

/**
 * Adding before position `at` (`0..=count`; `count` is the end).
 *
 * @param {ChapterPages} listing
 * @param {number} at
 */
export function insertRefusal(listing, at) {
  const next = identity(listing.pages.length)
  next.splice(at, 0, -1)
  return orderRefusal(listing, next)
}

/**
 * @param {ChapterPages} listing
 * @param {number[]} order
 */
export function moveRefusal(listing, order) {
  return orderRefusal(listing, order)
}

/**
 * The order that puts the selected pages, kept together and in their own
 * order, before position `target` of the current order. `target` may be the
 * page count, the end.
 *
 * @param {number} count
 * @param {number[]} selected
 * @param {number} target
 * @returns {number[]}
 */
export function orderMovingTo(count, selected, target) {
  const moving = [...new Set(selected)].filter((i) => i >= 0 && i < count).sort((a, b) => a - b)
  const rest = identity(count).filter((i) => !moving.includes(i))
  const before = rest.filter((i) => i < target).length
  rest.splice(before, 0, ...moving)
  return rest
}

/**
 * One step earlier (`-1`) or later (`1`) for the selected block.
 *
 * @param {number} count
 * @param {number[]} selected
 * @param {-1|1} direction
 * @returns {number[]|null} null when the block is already at that end
 */
export function orderStepping(count, selected, direction) {
  if (selected.length === 0) return null
  const first = Math.min(...selected)
  const last = Math.max(...selected)
  if (direction < 0 && first === 0) return null
  if (direction > 0 && last === count - 1) return null
  return orderMovingTo(count, selected, direction < 0 ? first - 1 : last + 2)
}

/**
 * What a page shows as its state on the screen.
 *
 * @param {import('../api/backend.js').ApiPage} page
 * @param {boolean} worked
 * @returns {'cleaned'|'masks'|'edited'|'untouched'}
 */
export function pageState(page, worked) {
  if (page.status === 'cleaned' || (page.doneCount ?? 0) > 0) return 'cleaned'
  if ((page.regionCount ?? 0) > 0) return 'masks'
  return worked ? 'edited' : 'untouched'
}

/**
 * What removing `positions` loses, for the confirmation: how many pages go
 * and how many of them carry work.
 *
 * @param {ChapterPages} listing
 * @param {number[]} positions
 */
export function removalLoss(listing, positions) {
  const worked = positions.filter((i) => listing.worked[i]).length
  return { pages: positions.length, worked }
}

/**
 * The current position of the page that was at `sourceIndex`, or the nearest
 * position that still exists - where an open editor goes after an edit.
 *
 * @param {Array<{sourceIndex?: number}>} pages
 * @param {number|undefined} sourceIndex
 * @param {number} fallback
 */
export function positionOfSource(pages, sourceIndex, fallback) {
  const found = sourceIndex === undefined ? -1 : pages.findIndex((page) => page.sourceIndex === sourceIndex)
  if (found >= 0) return found
  return Math.max(0, Math.min(fallback, pages.length - 1))
}
