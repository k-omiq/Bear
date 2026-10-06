/**
 * The masks of detected regions, as the canvas draws them: fetched from the
 * tile protocol, kept in a small cache, and coloured with canvas compositing.
 *
 * **What a detection looks like is what Clean will erase.** A box said where
 * the detector looked; the mask says which pixels the cleaner is handed - the
 * SAM text mask with its padding, and the lettering under it. The native side
 * serves it per region (`api/tile.js#detectionMaskUrl`) as a white-on-clear
 * PNG cropped to its own bounds, and says where those bounds sit on the page
 * in the `x-mask-bounds` header. That header is why this is a `fetch` and not
 * an `<img>`: an image element cannot read a response header.
 *
 * **The cache is keyed by URL alone.** The URL carries the mask's digest and
 * its edit sequence (`api/tile.js#detectionMaskUrl`), so an edit moves the URL
 * of exactly the masks it changed and no other mask on screen is fetched
 * again. An old URL's image ages out of the cache like any other.
 *
 * **Compositing only, never `getImageData`.** A per-pixel loop over a large
 * mask is slow on the main thread, and a canvas that reads its own pixels back
 * is one WebKit may refuse. `paintMask` builds the fill and the outline from
 * `drawImage`, `globalAlpha` and three composite operations instead.
 */

import { ImageCache, loadBoundedImage, parseBounds } from '../api/boundedimage.js'

/**
 * How many decoded masks are kept. A page carries tens of detections, and a
 * longstrip window holds a few pages at once; past that the least recently
 * drawn is dropped and fetched again if it comes back into view.
 */
export const MASK_CACHE_LIMIT = 96

/**
 * The largest backing store a mask canvas is given, in device pixels. WebKit
 * refuses a canvas much past this on some machines, and a long sound effect at
 * full zoom on a 2x display would ask for several times it. Past the cap the
 * canvas is drawn at a lower scale and stretched, which softens the edge and
 * keeps the outline its width.
 */
export const MAX_CANVAS_PIXELS = 4096 * 4096

/**
 * @typedef {import('../api/boundedimage.js').Bounds} MaskBounds - where a mask image sits on its page, in page pixels
 */

/**
 * @typedef {Object} MaskImage
 * @property {CanvasImageSource} image
 * @property {MaskBounds} bounds
 */

const cache = new ImageCache(MASK_CACHE_LIMIT)

/** Drop every cached mask: for the tests, which each start from nothing. */
export function forgetMaskImages() {
  cache.clear()
}

/** @returns {number} how many masks are cached, for the tests */
export function cachedMaskCount() {
  return cache.size
}

/** The `x-mask-bounds` header, `x,y,w,h` in whole page pixels (`api/boundedimage.js#parseBounds`). */
export const parseMaskBounds = parseBounds

/**
 * One mask, from the cache or the protocol (`api/boundedimage.js#loadBoundedImage`).
 *
 * @param {string} url
 * @param {{signal?: AbortSignal, fetcher?: typeof fetch, decode?: (blob: Blob) => Promise<CanvasImageSource>}} [options]
 * @returns {Promise<MaskImage>}
 */
export async function loadDetectionMask(url, options = {}) {
  return /** @type {Promise<MaskImage>} */ (loadBoundedImage(url, { ...options, cache, header: 'x-mask-bounds' }))
}

/**
 * The backing-store scale for a canvas shown at `width` by `height` CSS
 * pixels: the display's own ratio, lowered where that would pass
 * `MAX_CANVAS_PIXELS`.
 *
 * @param {number} width
 * @param {number} height
 * @param {number} [ratio]
 * @returns {number} device pixels per CSS pixel
 */
export function backingScale(width, height, ratio = globalThis.devicePixelRatio || 1) {
  const area = Math.max(1, width) * Math.max(1, height)
  return Math.max(0.01, Math.min(ratio, Math.sqrt(MAX_CANVAS_PIXELS / area)))
}

/** @type {HTMLCanvasElement|null} the outline is built here, then laid over the fill */
let scratch = null

/**
 * Colour a mask into a canvas whose backing store is already sized: a fill at
 * `fill` alpha, and an outline at full opacity just outside the shape.
 *
 * The mask is drawn into the canvas inset by `inset` on every side, which is
 * the room the outline needs: the image is cropped to its own bounds, so an
 * outline drawn inside it would be cut off at every edge the shape touches.
 *
 * The fill is the mask drawn at the fill's alpha with the colour then kept
 * only where it landed (`source-in`). The outline is the mask drawn at
 * `outline` pixels in a ring of directions, which is the shape grown by that
 * much, with the shape itself cut back out (`destination-out`) and the colour
 * kept where the ring is; it is built on a scratch canvas so the cut does not
 * reach the fill.
 *
 * @param {HTMLCanvasElement} canvas
 * @param {CanvasImageSource} image
 * @param {{inset: number, outline: number, color: string, fill: number}} style
 *   `inset` and `outline` in backing pixels, `fill` from 0 to 1
 * @returns {boolean} whether there was a context to draw into
 */
export function paintMask(canvas, image, { inset, outline, color, fill }) {
  const context = canvas?.getContext?.('2d') ?? null
  if (!context) return false
  const width = canvas.width
  const height = canvas.height
  const w = width - inset * 2
  const h = height - inset * 2
  context.globalCompositeOperation = 'source-over'
  context.globalAlpha = 1
  context.clearRect(0, 0, width, height)
  if (!(w > 0) || !(h > 0)) return true

  context.globalAlpha = Math.max(0, Math.min(1, fill))
  context.drawImage(image, inset, inset, w, h)
  context.globalAlpha = 1
  context.globalCompositeOperation = 'source-in'
  context.fillStyle = color
  context.fillRect(0, 0, width, height)
  context.globalCompositeOperation = 'source-over'

  if (!(outline > 0)) return true
  if (!scratch && typeof document !== 'undefined') scratch = document.createElement('canvas')
  const ringContext = scratch?.getContext?.('2d') ?? null
  if (!scratch || !ringContext) return true
  scratch.width = width
  scratch.height = height
  ringContext.globalCompositeOperation = 'source-over'
  ringContext.globalAlpha = 1
  ringContext.clearRect(0, 0, width, height)
  // Enough directions that the grown edge is round rather than an octagon at
  // the thicker, lit width.
  const steps = Math.min(24, Math.max(8, Math.ceil(outline * 4)))
  for (let step = 0; step < steps; step += 1) {
    const angle = (step / steps) * Math.PI * 2
    ringContext.drawImage(image, inset + Math.cos(angle) * outline, inset + Math.sin(angle) * outline, w, h)
  }
  ringContext.globalCompositeOperation = 'destination-out'
  ringContext.drawImage(image, inset, inset, w, h)
  ringContext.globalCompositeOperation = 'source-in'
  ringContext.fillStyle = color
  ringContext.fillRect(0, 0, width, height)
  ringContext.globalCompositeOperation = 'source-over'
  context.drawImage(scratch, 0, 0)
  return true
}
