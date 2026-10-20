/**
 * The live paint preview's two pieces of shared plumbing: the commit seam's
 * way to have a page's preview catch up before the draft is dropped, and the
 * cleaned tiles a clone preview is drawn from.
 */

const flushers = new Map()
export function registerPaintPreview(pageId, flush) {
  flushers.set(pageId, flush)
  return () => { if (flushers.get(pageId) === flush) flushers.delete(pageId) }
}
export async function flushPaintPreview(pageId) { await flushers.get(pageId)?.() }

/**
 * The cleaned tiles on a sheet, each with its place in fractions of the
 * sheet: what `paintrender.js#renderCloneTiles` draws. This is the page the
 * clone stamp reads (`paint/mod.rs#render_clone`), as it stood before the
 * stroke.
 *
 * @param {Element|null|undefined} sheet
 * @returns {Array<{image: HTMLImageElement, left: number, top: number, width: number, height: number}>}
 */
export function cleanedTiles(sheet) {
  const box = sheet?.getBoundingClientRect()
  if (!sheet || !box || !(box.width > 0) || !(box.height > 0)) return []
  return [...sheet.querySelectorAll('[data-artwork="cleaned"] img')]
    // A tile still loading its replacement is in the document twice; the
    // one on screen is the one that is not hidden.
    .filter((image) => image.complete && image.naturalWidth > 0 && image.style.visibility !== 'hidden')
    .map((image) => {
      const tile = image.getBoundingClientRect()
      return {
        image,
        left: (tile.left - box.left) / box.width,
        top: (tile.top - box.top) / box.height,
        width: tile.width / box.width,
        height: tile.height / box.height,
      }
    })
}
