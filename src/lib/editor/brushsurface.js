/**
 * The Brush's live pixels, drawn here as the pointer moves, with no round
 * trip to the native side.
 *
 * Each frame of the native preview opened the job, read and decoded the
 * whole page and rendered the stroke from its first point, so a fast drag
 * trailed far behind the pointer. A Brush stroke is one colour and needs no
 * page pixels to preview, so this file draws it from the same dab plan the
 * commit stamps (`paintplan.js#strokePlanner`) with the same coverage rule
 * (`crates/cleaner-core/src/paint/brush.rs#stamp_dab_subpixel`, analytic tip,
 * no build-up): a pixel's coverage is the largest any dab gave it, capped at
 * the stroke's opacity, so a soft rim stays soft however often the dabs
 * overlap. Only new dabs are stamped, so a frame costs what the pointer added.
 *
 * **On the tiles' grid.** The surface is sized like the page's proxy tiles
 * (`proxy.rs#ProxyPlan::for_page`: the short edge capped at 1024), so the
 * preview has the resolution the committed stroke will be shown at. Where the
 * proxy is smaller than the page, each proxy pixel averages a 2 by 2 grid of
 * samples, standing in for the box average the tiles are reduced with.
 *
 * What it does not match: the native renderer blends in linear light and the
 * canvas blends in sRGB, so a soft rim can read a little lighter or darker
 * until the committed tiles replace it.
 *
 * The buffers are kept in square tiles made on first touch, so a long stroke
 * over a tall strip page costs the area it covers and not the page's.
 *
 * **The clone stamp too.** A clone stroke is the same dabs with the rule
 * `paint/mod.rs#render_clone` stamps them by (`buildUp`): each dab moves a
 * pixel's coverage part of the way to the opacity, by the dab's flow, so slow
 * strokes build up. Its pixels are the page itself, so the coverage is not
 * shown as a colour: `paintClone` draws the page's own tiles, moved by the
 * clone's offset, and keeps them only where the coverage is.
 */

import { strokePlanner } from './paintplan.js'
import { renderCloneTiles } from './paintrender.js'

/** Matches `proxy.rs#PROXY_SHORT_EDGE`. */
export const PROXY_SHORT_EDGE = 1024
const TILE = 128
const EPSILON = 1e-6
/** The smallest dab drawn, in surface pixels, so a thin stroke never vanishes. */
const MIN_RADIUS = 0.75

/**
 * @param {number} pageWidth
 * @param {number} pageHeight
 * @returns {number} surface pixels per page pixel
 */
export function proxyScale(pageWidth, pageHeight) {
  const short = Math.max(1, Math.min(Number(pageWidth) || 1, Number(pageHeight) || 1))
  return Math.min(1, PROXY_SHORT_EDGE / short)
}

/** `brush.rs#analytic_radial_coverage`, `t` in radii from the centre. */
export function coverageAt(t, hardness) {
  const r = Math.abs(t)
  if (r >= 1) return 0
  const h = Math.min(1, Math.max(0, hardness))
  if (h >= 1 - EPSILON || r <= h) return 1
  const span = 1 - h
  if (span <= EPSILON) return 1
  const x = Math.min(1, Math.max(0, (r - h) / span))
  return x <= 0.55 ? 1 - (x / 0.55) * 0.45 : 0.55 * (1 - (x - 0.55) / 0.45)
}

/** @param {string} hex */
function rgbOf(hex) {
  const value = String(hex ?? '').trim().replace(/^#/, '')
  const full = value.length === 3 ? [...value].map((c) => c + c).join('') : value
  const n = /^[0-9a-f]{6}$/i.test(full) ? parseInt(full, 16) : 0
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255]
}

/**
 * @typedef {Object} SurfaceTile
 * @property {number} tx
 * @property {number} ty
 * @property {Float32Array} mask - the stroke coverage so far, `brush.rs`'s `stroke_mask`
 * @property {Float32Array} alpha - the accumulated alpha, `brush.rs`'s `color[3]`
 * @property {Uint8ClampedArray} rgba
 * @property {boolean} dirty
 */

/**
 * One stroke's surface.
 *
 * @param {{
 *   brush: import('./paintplan.js').BrushSpec,
 *   color: string,
 *   pageWidth: number,
 *   pageHeight: number,
 *   clipY?: boolean,
 *   buildUp?: boolean,
 * }} options - `clipY` false lets a longstrip stroke run past the page's top and bottom;
 *   `buildUp` is the clone stamp's rule, and its alpha is the coverage itself,
 *   which is all `render_clone` keeps of the stroke
 */
export function brushSurface({ brush, color, pageWidth, pageHeight, clipY = true, buildUp = false }) {
  const scale = proxyScale(pageWidth, pageHeight)
  const samples = scale < 1 - EPSILON ? 2 : 1
  const width = Math.max(1, Math.round(pageWidth * scale))
  const height = Math.max(1, Math.round(pageHeight * scale))
  const opacity = Math.min(1, Math.max(0, brush.opacity / 100))
  const hardness = Math.min(1, Math.max(0, brush.hardness / 100))
  const [r, g, b] = rgbOf(color)
  const plan = strokePlanner(brush)
  /** @type {Map<string, SurfaceTile>} */
  const tiles = new Map()
  /** Touched tiles' extent, in tile units, inclusive. */
  let extent = /** @type {{x0: number, y0: number, x1: number, y1: number}|null} */ (null)
  /** What the canvas was last sized and placed for. */
  let shownFor = ''
  /** @type {HTMLCanvasElement|null} */
  let shownOn = null

  /** @returns {SurfaceTile} */
  function tileAt(tx, ty) {
    const key = `${tx},${ty}`
    let tile = tiles.get(key)
    if (!tile) {
      tile = {
        tx,
        ty,
        mask: new Float32Array(TILE * TILE),
        alpha: new Float32Array(TILE * TILE),
        rgba: new Uint8ClampedArray(TILE * TILE * 4),
        dirty: true,
      }
      tiles.set(key, tile)
      extent = extent
        ? { x0: Math.min(extent.x0, tx), y0: Math.min(extent.y0, ty), x1: Math.max(extent.x1, tx), y1: Math.max(extent.y1, ty) }
        : { x0: tx, y0: ty, x1: tx, y1: ty }
    }
    return tile
  }

  /** @param {import('./paintplan.js').PlannedDab} dab - in page pixels */
  function stamp(dab) {
    const alpha = Math.min(1, Math.max(0, dab.alpha))
    if (alpha <= 0 || opacity <= 0) return
    const cx = dab.x * scale
    const cy = dab.y * scale
    const radius = Math.max(dab.radius * scale, MIN_RADIUS)
    const reach = radius + 1
    const minX = Math.max(0, Math.floor(cx - reach))
    const maxX = Math.min(width - 1, Math.ceil(cx + reach))
    const minY = clipY ? Math.max(0, Math.floor(cy - reach)) : Math.floor(cy - reach)
    const maxY = clipY ? Math.min(height - 1, Math.ceil(cy + reach)) : Math.ceil(cy + reach)
    for (let py = minY; py <= maxY; py += 1) {
      const ty = Math.floor(py / TILE)
      const row = (py - ty * TILE) * TILE
      for (let px = minX; px <= maxX; px += 1) {
        // `brush.rs` samples a page pixel on its integer coordinate, so page
        // pixel k spans k - 0.5 to k + 0.5, and a surface pixel spans
        // `1 / scale` of them. At scale 1 this is exactly `px - cx`.
        let covered = 0
        for (let sy = 0; sy < samples; sy += 1) {
          const dy = py + (sy + 0.5) / samples - 0.5 * scale - cy
          for (let sx = 0; sx < samples; sx += 1) {
            const dx = px + (sx + 0.5) / samples - 0.5 * scale - cx
            covered += coverageAt(Math.hypot(dx, dy) / radius, hardness)
          }
        }
        const dabAlpha = Math.min(1, (covered / (samples * samples)) * alpha)
        if (dabAlpha <= 0) continue
        const tx = Math.floor(px / TILE)
        const tile = tileAt(tx, ty)
        const i = row + (px - tx * TILE)
        const prev = tile.mask[i]
        const o = i * 4
        if (buildUp) {
          const effective = (opacity - prev) * dabAlpha
          if (effective <= EPSILON) continue
          const next = Math.min(opacity, prev + effective)
          tile.mask[i] = next
          tile.rgba[o + 3] = Math.round(next * 255)
          tile.dirty = true
          continue
        }
        const target = Math.min(opacity, Math.max(prev, dabAlpha))
        const effective = target - prev
        tile.mask[i] = target
        if (effective <= EPSILON) continue
        const a = effective + tile.alpha[i] * (1 - effective)
        tile.alpha[i] = a
        tile.rgba[o] = r
        tile.rgba[o + 1] = g
        tile.rgba[o + 2] = b
        tile.rgba[o + 3] = Math.round(Math.min(1, a) * 255)
        tile.dirty = true
      }
    }
  }

  return {
    /**
     * The next point of the stroke, in page pixels.
     *
     * @param {{x: number, y: number, p?: number}} point
     */
    add(point) {
      for (const dab of plan(point)) stamp(dab)
    },

    /**
     * Where the canvas sits, in page percent: the touched tiles' box.
     *
     * @returns {{x: number, y: number, w: number, h: number}|null}
     */
    bounds() {
      if (!extent) return null
      const toX = (/** @type {number} */ v) => (v / scale / pageWidth) * 100
      const toY = (/** @type {number} */ v) => (v / scale / pageHeight) * 100
      return {
        x: toX(extent.x0 * TILE),
        y: toY(extent.y0 * TILE),
        w: toX((extent.x1 - extent.x0 + 1) * TILE),
        h: toY((extent.y1 - extent.y0 + 1) * TILE),
      }
    },

    /**
     * Bring a canvas up to date: the tiles that changed since the last call,
     * or all of them where the canvas is new or had to grow.
     *
     * @param {HTMLCanvasElement} node
     */
    paint(node) {
      const box = this.bounds()
      if (!box || !extent) return
      const key = `${extent.x0},${extent.y0},${extent.x1},${extent.y1}`
      const fresh = node !== shownOn || key !== shownFor
      if (fresh) {
        // A canvas's size cannot change without clearing it, so a stroke that
        // grows past its tiles is put back whole.
        node.width = (extent.x1 - extent.x0 + 1) * TILE
        node.height = (extent.y1 - extent.y0 + 1) * TILE
        node.style.left = `${box.x}%`
        node.style.top = `${box.y}%`
        node.style.width = `${box.w}%`
        node.style.height = `${box.h}%`
        shownOn = node
        shownFor = key
      }
      const context = node.getContext('2d')
      if (!context?.putImageData) return
      for (const tile of tiles.values()) {
        if (!fresh && !tile.dirty) continue
        tile.dirty = false
        context.putImageData(
          new ImageData(tile.rgba, TILE, TILE),
          (tile.tx - extent.x0) * TILE,
          (tile.ty - extent.y0) * TILE,
        )
      }
    },

    /**
     * Bring a canvas up to date with a clone stroke: the page's tiles, moved
     * by the clone's offset, kept where the coverage is (`destination-in`).
     * The coverage goes onto `mask`, an offscreen canvas of this stroke's own,
     * a changed tile at a time; the page is drawn whole each time, which is a
     * `drawImage` per tile and no readback, so `tile://` images tainting the
     * canvas does not matter.
     *
     * @param {HTMLCanvasElement} node
     * @param {{
     *   mask: HTMLCanvasElement,
     *   tiles: Parameters<typeof renderCloneTiles>[1],
     *   shift: {x: number, y: number},
     * }} clone - `tiles` are the sheet's, in fractions of the sheet; `shift`
     *   is source minus target, in whole page pixels
     */
    paintClone(node, { mask, tiles, shift }) {
      this.paint(mask)
      const box = this.bounds()
      if (!box || !extent) return
      if (node.width !== mask.width || node.height !== mask.height) {
        node.width = mask.width
        node.height = mask.height
      }
      node.style.left = `${box.x}%`
      node.style.top = `${box.y}%`
      node.style.width = `${box.w}%`
      node.style.height = `${box.h}%`
      const context = node.getContext('2d')
      if (!context) return
      context.clearRect(0, 0, node.width, node.height)
      renderCloneTiles(context, tiles, {
        width,
        height,
        dx: -(extent.x0 * TILE + shift.x * scale),
        dy: -(extent.y0 * TILE + shift.y * scale),
      })
      context.globalCompositeOperation = 'destination-in'
      context.drawImage(mask, 0, 0)
      context.globalCompositeOperation = 'source-over'
    },
  }
}
