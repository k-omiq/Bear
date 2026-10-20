/**
 * The eraser's live preview: Brush layers thinning under the stroke, before
 * the committed layers load.
 *
 * The eraser's coverage is drawn by `PaintLayer.svelte` on a surface like the
 * Brush's (`brushsurface.js`: same dab plan, same tip, no build-up, capped at
 * the opacity), onto a canvas that is never shown. Each Brush layer of the
 * page (`PatchLayer.svelte`) draws its own picture again and takes that
 * coverage off it with `destination-out`, which scales its alpha by
 * `1 - coverage`: `paint/erase.rs#erase_patch`'s `old * (255 - e) / 255`.
 *
 * **A stroke is kept per layer picture.** A layer remembers the picture it
 * had when a stroke first reached it, and takes that stroke off only that
 * picture: once the commit gives the layer its next picture, the erase is in
 * its pixels and must not be taken off again. So a stroke can stay shown
 * while its commit lands, layer by layer, with no blink back to the paint.
 */

import { SvelteMap } from 'svelte/reactivity'

/**
 * @typedef {Object} EraseMask
 * @property {number} id - the stroke's
 * @property {CanvasImageSource & {width: number, height: number}} canvas - the coverage, as alpha
 * @property {{x: number, y: number, w: number, h: number}} box - where `canvas` lies, in page percent
 * @property {number} tick - moves each time `canvas` has more on it
 */

/** The eraser strokes shown on each page, by page id, oldest first. */
export const eraseMasks = new SvelteMap()

/**
 * Show a stroke on a page, or move its tick.
 *
 * @param {string} pageId
 * @param {EraseMask} mask
 */
export function showEraseMask(pageId, mask) {
  const list = eraseMasks.get(pageId) ?? []
  const known = list.some((shown) => shown.id === mask.id)
  eraseMasks.set(pageId, known ? list.map((shown) => (shown.id === mask.id ? mask : shown)) : [...list, mask])
}

/**
 * Keep only the strokes whose ids are in `ids`.
 *
 * @param {string} pageId
 * @param {ReadonlySet<number>} ids
 */
export function keepEraseMasks(pageId, ids) {
  const list = eraseMasks.get(pageId)
  if (!list) return
  const kept = list.filter((mask) => ids.has(mask.id))
  if (kept.length === 0) eraseMasks.delete(pageId)
  else if (kept.length !== list.length) eraseMasks.set(pageId, kept)
}

/**
 * Draw a layer's picture with the eraser strokes taken off it, on a canvas
 * that is clear.
 *
 * @param {CanvasRenderingContext2D} context - the layer's canvas, sized to `bounds`
 * @param {CanvasImageSource} image - the layer's picture
 * @param {ReadonlyArray<EraseMask>} masks
 * @param {{x: number, y: number, w: number, h: number}} bounds - the layer's, in proxy pixels
 * @param {{width: number, height: number}} plan - the page's proxy size
 */
export function drawErased(context, image, masks, bounds, plan) {
  context.drawImage(image, 0, 0)
  if (masks.length === 0) return
  context.globalCompositeOperation = 'destination-out'
  for (const { canvas, box } of masks) {
    context.drawImage(
      canvas,
      (box.x / 100) * plan.width - bounds.x,
      (box.y / 100) * plan.height - bounds.y,
      (box.w / 100) * plan.width,
      (box.h / 100) * plan.height,
    )
  }
  context.globalCompositeOperation = 'source-over'
}
