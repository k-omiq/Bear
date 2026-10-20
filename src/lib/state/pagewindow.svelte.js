/**
 * The resident page window.
 *
 * ## What was already lazy, and what was not
 *
 * Pixels have never been the problem. A page's bytes reach the webview over the
 * `tile://` protocol as proxy tiles, tiles inside a page are
 * `loading="lazy"`, and `src/lib/editor/strip.js` mounts only the visible band
 * of a longstrip column plus one page of overscan. Nothing in this module
 * touches any of that, and nothing in it needs to.
 *
 * What sat in RAM for a whole chapter was **region state**: `ApiRegion` objects,
 * one per detected bubble, each carrying a mask reference and a provenance
 * record, all of them arriving in one payload when a chapter opened and none of
 * them ever leaving. A 200-page chapter at twenty regions a page is four
 * thousand of those, they are `$state`-proxied the moment they land in
 * `editor.chapter`, and every one of them is live for the session. On a
 * low-memory machine that is the difference this module is about.
 *
 * ## The window
 *
 * **Previous, current, next.** Three pages' regions are resident (eleven in a
 * longstrip, `STRIP_RADIUS`); everything
 * else is a header - a name, a size, a status and a region *count*. Moving to a
 * page loads it and its neighbours and drops what has left, so the cost is flat
 * in chapter length rather than linear in it.
 *
 * In a longstrip chapter the window is that band **union the strip's own
 * scope** - the positions the column actually has on screen, which the canvas
 * reports through `setStripScope`. Those are the pages the Layers panel is
 * listing (`scopePageIndices`), so a window that did not contain them would
 * make the panel empty at exactly the moment it is being read. The strip's own
 * virtualisation already bounds that band, so this cannot grow without bound;
 * it is "three pages, or what is on screen, whichever is the larger claim".
 *
 * ## Nothing here has to be flushed
 *
 * A page is evicted by dropping its regions, and that is safe because **the
 * interface never holds an unsaved region**. Every edit goes through the seam
 * and is written to the manifest before the interface applies its own copy -
 * `maskactions.svelte.js` says so at the top and every call site obeys it - so
 * a resident region is a *cache of a file*, never the only copy. The one thing
 * that used to be session-only, the undo history, is now a journal on disk too
 * which is what makes the eviction actually give memory back:
 * before that, a closure on the undo stack kept the region alive anyway.
 *
 * ## Concurrency
 *
 * A window slide is async and page turns are not. Two slides in flight would
 * race, and the loser would write its regions into a chapter that has moved on.
 * So each slide takes a token, and a slide whose token has been superseded
 * applies nothing.
 */

import { getBackend } from '../api/backend.js'
import { recountPage } from '../model/status.js'
import { KEEP_PAGES } from '../editor/strip.js'

/** Pages either side of the current one. Three resident pages in all. */
export const WINDOW_RADIUS = 1

/**
 * In a longstrip the radius is the strip's warm range instead (`KEEP_PAGES`
 * in `editor/strip.js`): the pages kept mounted are the pages whose regions
 * are in hand, so a mask that hangs from the page above across a join is
 * there whenever the page below is.
 */
export const STRIP_RADIUS = KEEP_PAGES

let slideToken = 0

/**
 * The page indices that should be resident, given where the reader is.
 *
 * Pure, and exported for the tests: the window's arithmetic is the part worth
 * asserting, and it does not need a backend to assert it.
 *
 * @param {{pageIndex?: number, pageCount?: number, stripScope?: number[], longstrip?: boolean}} spec
 * @returns {number[]} in ascending order, clamped to the chapter
 */
export function windowIndices(spec) {
  const count = Math.max(0, Math.trunc(spec.pageCount ?? 0))
  if (count === 0) return []
  const last = count - 1
  const centre = Math.min(last, Math.max(0, Math.trunc(spec.pageIndex ?? 0)))

  const wanted = new Set()
  const radius = spec.longstrip ? STRIP_RADIUS : WINDOW_RADIUS
  for (let offset = -radius; offset <= radius; offset += 1) {
    const index = centre + offset
    if (index >= 0 && index <= last) wanted.add(index)
  }
  if (spec.longstrip) {
    for (const index of spec.stripScope ?? []) {
      const position = Math.trunc(index)
      if (position >= 0 && position <= last) wanted.add(position)
    }
  }
  return [...wanted].sort((a, b) => a - b)
}

/**
 * Which of `wanted` still has to be fetched, and which resident pages should be
 * dropped.
 *
 * @param {Array<{index: number, resident?: boolean}>} pages
 * @param {number[]} wanted
 * @returns {{load: number[], evict: number[]}}
 */
export function windowPlan(pages, wanted) {
  const keep = new Set(wanted)
  const load = []
  const evict = []
  for (const page of pages) {
    if (keep.has(page.index)) {
      if (!page.resident) load.push(page.index)
    } else if (page.resident) {
      evict.push(page.index)
    }
  }
  return { load, evict }
}

/**
 * Slide the window to wherever the reader now is.
 *
 * `store` is the editor state module's own handle on the open chapter, passed
 * in rather than imported so that this module has no cycle with the one that
 * calls it on every page turn.
 *
 * @param {{
 *   chapterId: string,
 *   pages: Array<{index: number, resident?: boolean, regions?: Array<Object>}>,
 *   wanted: number[],
 *   applyPage: (page: Object) => void,
 *   evictPage: (index: number) => void,
 * }} store
 * @returns {Promise<void>}
 */
export async function slideWindow(store) {
  slideToken += 1
  const token = slideToken
  const { load, evict } = windowPlan(store.pages, store.wanted)

  // Eviction first, and synchronously: it is the half that gives memory back,
  // it cannot fail, and doing it before the fetch means the peak is the window
  // rather than the window plus what it replaced.
  for (const index of evict) store.evictPage(index)
  if (load.length === 0) return

  const loaded = await getBackend().loadPages({ chapterId: store.chapterId, indices: load })
  // A slide that has been superseded applies nothing: the reader has moved, the
  // window it was fetching is no longer the window, and writing these pages in
  // would resurrect regions the newer slide has just evicted.
  if (token !== slideToken) return
  for (const page of loaded ?? []) store.applyPage(page)
}

/**
 * Reduce a page to its header. The inverse of a load, and the only place a
 * region leaves the interface without the backend having been told.
 *
 * @param {Object} page
 * @returns {Object} the same page object, mutated in place so `$state` sees it
 */
export function evictRegions(page) {
  // The counts outlive the regions: they are what the Pages list draws for
  // every page the window is not holding, so they are taken off the list on
  // the way out rather than lost with it.
  // `resident` or a non-empty list, because a `page-done` payload carries the
  // whole page without necessarily claiming residency, and its counts are the
  // freshest thing there is.
  if (page.resident || page.regions?.length) recountPage(page)
  page.regions = []
  page.resident = false
  return page
}
