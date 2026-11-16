/**
 * Where the pixel grid's lines go. Pure - no Svelte, no DOM.
 *
 * The grid used to be one CSS background square, repeated. WebKit rounds a
 * repeated background's size to a device pixel (`39.69px` is drawn `39.5px`
 * on a 2x screen), and the error adds up: a thousand image pixels in, the
 * grid was whole squares off the image. Here every line is placed by
 * itself, at the device pixel its own image pixel starts on, so no error is
 * carried from one line to the next.
 *
 * Only the part of the page on screen is drawn. A page at 12800% is hundreds
 * of thousands of device pixels long, which no canvas can be.
 */

/** How far past the viewport, in CSS pixels, a strip is drawn ahead of a scroll. */
export const GRID_OVERSCAN = 256

/**
 * One axis of the grid, in whole device pixels of the viewport.
 *
 * @param {{edge: number, pixel: number, count: number, view: number, dpr: number, overscan?: number}} spec
 *   `edge` is where the sheet starts and `view` how long the viewport is,
 *   both in CSS pixels; `pixel` is CSS pixels per image pixel and `count`
 *   the image pixels along the axis.
 * @returns {{start: number, length: number, lines: number[], seenFrom: number, seenTo: number}|null}
 *   `start` is the strip's first device pixel and `length` how many it has;
 *   `lines` are offsets inside the strip; `seenFrom` and `seenTo` bound the
 *   part of the sheet inside the viewport itself. `null` when none of the
 *   sheet is near the viewport.
 */
export function gridAxis(spec) {
  const dpr = spec.dpr > 0 ? spec.dpr : 1
  const count = Math.floor(spec.count)
  if (!(spec.pixel > 0) || !(count > 0) || !Number.isFinite(spec.edge) || !(spec.view > 0)) return null
  // The browser draws the page from one whole device pixel to another, and
  // the image pixels share what is between. The lines do the same.
  const edge = Math.round(spec.edge * dpr)
  const far = Math.round((spec.edge + count * spec.pixel) * dpr)
  const step = (far - edge) / count
  if (!(step > 0)) return null
  const overscan = Math.round((spec.overscan ?? GRID_OVERSCAN) * dpr)
  const view = Math.ceil(spec.view * dpr)
  const start = Math.max(edge, -overscan)
  const length = Math.min(far, view + overscan) - start
  if (!(length > 0)) return null
  const first = Math.max(0, Math.floor((start - edge) / step))
  const last = Math.min(count - 1, Math.ceil((start + length - edge) / step))
  const lines = []
  for (let index = first; index <= last; index += 1) {
    // The device pixel a nearest-neighbour image pixel starts on.
    const at = Math.round(edge + index * step) - start
    if (at >= 0 && at < length) lines.push(at)
  }
  return { start, length, lines, seenFrom: Math.max(edge, 0), seenTo: Math.min(far, view) }
}

/**
 * Whether a strip drawn earlier still covers what is on screen now. The
 * strip moves with the sheet, so both are measured from the sheet's edge.
 *
 * @param {{from: number, to: number}|null} drawn the strip, from the sheet's edge, in device pixels
 * @param {{start: number, seenFrom: number, seenTo: number}} axis
 * @param {number} edge the sheet's edge now, in device pixels
 * @returns {boolean}
 */
export function gridCovers(drawn, axis, edge) {
  if (!drawn) return false
  return drawn.from <= axis.seenFrom - edge && drawn.to >= axis.seenTo - edge
}
