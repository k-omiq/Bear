/**
 * A page's patches, as the editor draws them: one see-through image per
 * patch, over the page's `source` tiles (`PatchLayers.svelte`).
 *
 * **Why layers and not the flattened page.** A `cleaned` tile is the page
 * with every patch composited into it, so its URL named every patch, and one
 * clean moved the URL of every cleaned tile on the page. The webview blanks
 * an image whose `src` changed until the new bytes decode, and the source
 * underneath showed through: every cleaned spot flashed its lettering on
 * every edit. A layer's URL names one patch (`api/tile.js#layerUrl`), so an
 * edit fetches one small image and nothing else on the page is touched; and
 * the canvas that shows it keeps its old pixels until the new ones are
 * decoded and drawn in one step.
 *
 * **Which patches a page draws.** Its own, and in a longstrip a neighbour's
 * whose box reaches across the join (`model/layers.js#pagesReached`, one
 * pixel generous; the protocol answers `204` for a guess that did not land).
 * Both come from resident pages' regions: the window keeps every mounted page
 * and one neighbour either side resident (`state/editor.svelte.js#residentIndices`).
 * They are stacked in compositing order, `(order, id)`, as
 * `cleaner_core::composite` stamps them.
 */

import { SvelteMap } from 'svelte/reactivity'
import { ImageCache, loadBoundedImage } from '../api/boundedimage.js'
import { layerUrl } from '../api/tile.js'

/**
 * Bytes of decoded layers kept. Layers are proxy-sized crops of single
 * patches, so this holds several pages' worth; an undo that brings an old
 * picture back usually finds it here.
 */
export const LAYER_CACHE_BYTES = 160 * 1024 * 1024

/** The header naming where a layer sits, in proxy pixels (`tile.rs`). */
export const LAYER_BOUNDS_HEADER = 'x-layer-bounds'

const cache = new ImageCache(LAYER_CACHE_BYTES, (entry) => {
  const image = /** @type {{width?: number, height?: number}|null} */ (entry.image)
  return image ? Math.max(1024, (image.width ?? 0) * (image.height ?? 0) * 4) : 64
})

/** @type {Map<string, Promise<import('../api/boundedimage.js').BoundedImage>>} */
const pending = new Map()

/**
 * How many layer draws each page has had, by page id: what a paint stroke's
 * held preview waits on (`PaintLayer.svelte`), because a draw is the moment
 * the committed pixels are on the sheet.
 */
export const layerDraws = new SvelteMap()

/** @param {string} pageId */
export function noteLayerDrawn(pageId) {
  layerDraws.set(pageId, (layerDraws.get(pageId) ?? 0) + 1)
}

/** Drop every cached layer: for the tests, which each start from nothing. */
export function forgetLayerImages() {
  cache.clear()
  pending.clear()
  layerDraws.clear()
}

/**
 * A layer already decoded, or `undefined`: read synchronously, so a sheet
 * mounted again draws what it had in the same frame.
 *
 * @param {string} url
 */
export function cachedLayer(url) {
  return cache.get(url)
}

/**
 * One layer, from the cache or the protocol. A request already in flight is
 * shared rather than repeated, so warming a page and mounting it cost one
 * fetch. Nothing is aborted: a layer is a small local response, and the next
 * sheet to want it finds it cached.
 *
 * @param {string} url
 * @param {{fetcher?: typeof fetch, decode?: (blob: Blob) => Promise<CanvasImageSource>}} [options]
 * @returns {Promise<import('../api/boundedimage.js').BoundedImage>}
 */
export function loadLayer(url, options = {}) {
  const hit = cache.get(url)
  if (hit) return Promise.resolve(hit)
  let request = pending.get(url)
  if (!request) {
    request = loadBoundedImage(url, { ...options, cache, header: LAYER_BOUNDS_HEADER, empty: true })
      .finally(() => pending.delete(url))
    pending.set(url, request)
  }
  return request
}

/**
 * @typedef {Object} PageLayer
 * @property {string} id - the region's id
 * @property {string} url
 * @property {number} order
 * @property {number} opacity - `0..1`, drawn by the canvas
 * @property {boolean} own - whether the patch belongs to this page, rather than reaching it across a join
 * @property {{x: number, y: number, w: number, h: number}} bbox - page percent of the page it belongs to
 */

/**
 * The layers page `page` draws, bottom first.
 *
 * `list` is the chapter's pages in strip order when the chapter is a
 * longstrip, and just `[page]` when it is not. Only a region with a patch has
 * a layer; a detection draws nothing on the cleaned side.
 *
 * @param {import('../api/backend.js').ApiPage} page
 * @param {ReadonlyArray<import('../api/backend.js').ApiPage>} list
 * @returns {PageLayer[]}
 */
export function pageLayers(page, list) {
  const at = list.indexOf(page)
  const heights = list.map((candidate) => Number(candidate.height))
  const offsets = []
  let top = 0
  for (const height of heights) {
    offsets.push(top)
    top += height
  }
  const known = heights.every((height) => height > 0)

  /** @type {PageLayer[]} */
  const layers = []
  list.forEach((anchor, index) => {
    for (const region of anchor.regions ?? []) {
      if (!region.mask || region.outcome === 'detected') continue
      const own = anchor === page
      if (!own && !(known && reaches(offsets, heights, index, at, region.bbox))) continue
      const url = layerUrl(page, region, anchor)
      if (!url) continue
      layers.push({
        id: region.id,
        url,
        order: Number(region.mask.order ?? 0),
        opacity: Math.min(Math.max(Number(region.mask.layer?.opacity ?? 100), 0), 100) / 100,
        own,
        bbox: region.bbox,
      })
    }
  })
  return layers.sort((a, b) => a.order - b.order || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
}

/**
 * Whether a box on strip page `anchor` reaches page `target`, a pixel
 * generous: `model/layers.js#pagesReached` for one pair, over offsets
 * computed once per page rather than once per region.
 *
 * @param {number[]} offsets
 * @param {number[]} heights
 * @param {number} anchor
 * @param {number} target
 * @param {{y: number, h: number}|undefined} box
 */
function reaches(offsets, heights, anchor, target, box) {
  if (!box || target < 0) return false
  const y0 = offsets[anchor] + (box.y / 100) * heights[anchor]
  const y1 = y0 + (box.h / 100) * heights[anchor]
  return offsets[target] < y1 + 1 && offsets[target] + heights[target] > y0 - 1
}

/**
 * Fetch a page's layers ahead of drawing it, into the same cache the sheet
 * reads, so a page turn draws pixels that are already here.
 *
 * @param {ReadonlyArray<PageLayer>} layers
 */
export function warmLayers(layers) {
  for (const layer of layers) loadLayer(layer.url).catch(() => {})
}
