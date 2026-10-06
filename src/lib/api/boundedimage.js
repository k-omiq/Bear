/**
 * Images the tile protocol serves cropped to their own bounds, with the
 * bounds in a response header: a detection's mask (`x-mask-bounds`, page
 * pixels) and a patch's layer (`x-layer-bounds`, proxy pixels).
 *
 * **A `fetch` and not an `<img>`**, because an image element cannot read a
 * response header, and because decoding off the DOM is what lets a caller
 * keep the picture already on screen until the new one is ready to draw.
 *
 * **The cache is keyed by URL alone.** Each URL carries the digest of what it
 * draws (`api/tile.js`), so an edit moves the URL of exactly the images it
 * changed; an old URL's image ages out like any other.
 */

/**
 * @typedef {Object} Bounds - where an image sits, in the pixels its header names
 * @property {number} x
 * @property {number} y
 * @property {number} w
 * @property {number} h
 */

/**
 * @typedef {Object} BoundedImage
 * @property {CanvasImageSource|null} image - `null` for a `204`: nothing to draw
 * @property {Bounds|null} bounds
 */

/**
 * A bounds header, `x,y,w,h` in whole pixels, or `null` when it is missing or
 * is not four numbers with a positive width and height.
 *
 * @param {string|null|undefined} header
 * @returns {Bounds|null}
 */
export function parseBounds(header) {
  if (typeof header !== 'string') return null
  const parts = header.split(',').map((part) => part.trim())
  if (parts.length !== 4 || parts.some((part) => !/^-?\d+(\.\d+)?$/.test(part))) return null
  const [x, y, w, h] = parts.map(Number)
  if (!(w > 0) || !(h > 0)) return null
  return { x, y, w, h }
}

/**
 * Least recently used first, bounded by a total weight: a count when every
 * entry weighs one, bytes when `weigh` says so.
 */
export class ImageCache {
  /**
   * @param {number} limit
   * @param {(entry: BoundedImage) => number} [weigh]
   */
  constructor(limit, weigh = () => 1) {
    this.limit = limit
    this.weigh = weigh
    /** @type {Map<string, {entry: BoundedImage, weight: number}>} insertion order is recency order */
    this.entries = new Map()
    this.weight = 0
  }

  /** @returns {number} */
  get size() {
    return this.entries.size
  }

  /**
   * The entry, touched so the recency order is the order images were last
   * asked for.
   *
   * @param {string} url
   * @returns {BoundedImage|undefined}
   */
  get(url) {
    const hit = this.entries.get(url)
    if (!hit) return undefined
    this.entries.delete(url)
    this.entries.set(url, hit)
    return hit.entry
  }

  /**
   * @param {string} url
   * @param {BoundedImage} entry
   */
  set(url, entry) {
    const old = this.entries.get(url)
    if (old) this.weight -= old.weight
    this.entries.delete(url)
    const weight = this.weigh(entry)
    this.entries.set(url, { entry, weight })
    this.weight += weight
    // The entry just set is never the one evicted.
    while (this.weight > this.limit && this.entries.size > 1) {
      const oldest = /** @type {string} */ (this.entries.keys().next().value)
      this.weight -= /** @type {{weight: number}} */ (this.entries.get(oldest)).weight
      this.entries.delete(oldest)
    }
  }

  clear() {
    this.entries.clear()
    this.weight = 0
  }
}

/**
 * One image, from `cache` or the protocol.
 *
 * Rejects on a failed response, a missing bounds header, an image that will
 * not decode, and an abort; the caller tells the last apart by `signal`. With
 * `empty`, a `204` is an answer rather than a failure: nothing to draw, and
 * cached as such.
 *
 * @param {string} url
 * @param {{
 *   cache: ImageCache,
 *   header: string,
 *   empty?: boolean,
 *   signal?: AbortSignal,
 *   fetcher?: typeof fetch,
 *   decode?: (blob: Blob) => Promise<CanvasImageSource>,
 * }} options
 * @returns {Promise<BoundedImage>}
 */
export async function loadBoundedImage(url, { cache, header, empty = false, signal, fetcher, decode = decodeImage }) {
  const hit = cache.get(url)
  if (hit) return hit
  const request = fetcher ?? globalThis.fetch
  if (typeof request !== 'function') throw new Error('image_fetch_unavailable')
  const response = await request(url, signal ? { signal } : undefined)
  if (empty && response.status === 204) {
    const nothing = { image: null, bounds: null }
    cache.set(url, nothing)
    return nothing
  }
  if (!response.ok) throw new Error(`image_fetch_failed: ${response.status}`)
  const bounds = parseBounds(response.headers?.get?.(header))
  if (!bounds) throw new Error('image_bounds_missing')
  const image = await decode(await response.blob())
  if (signal?.aborted) throw new DOMException('aborted', 'AbortError')
  const entry = { image, bounds }
  cache.set(url, entry)
  return entry
}

/**
 * Decode a PNG blob into something a canvas can draw.
 *
 * `createImageBitmap` where the engine has it. The WebKit under macOS 11
 * shipped without it until Safari 15, so an `Image` over an object URL is the
 * route there; `decode()` is what makes it drawable before it is in the DOM.
 *
 * @param {Blob} blob
 * @returns {Promise<CanvasImageSource>}
 */
export async function decodeImage(blob) {
  if (typeof globalThis.createImageBitmap === 'function') return globalThis.createImageBitmap(blob)
  const url = URL.createObjectURL(blob)
  try {
    const image = new Image()
    image.src = url
    await image.decode()
    return image
  } finally {
    URL.revokeObjectURL(url)
  }
}
