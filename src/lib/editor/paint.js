/**
 * Construction of `params.paint` for brush (paint mode), the clone stamp, and a
 * Shapes gesture filled with a solid colour.
 *
 * Shared between drawing gestures (`drawing.svelte.js`) and tool applications
 * (`toolapply.svelte.js`), so both produce the exact same seam payload.
 */

/**
 * @typedef {Object} PaintPoint
 * @property {number} x - page percent 0..100
 * @property {number} y - page percent 0..100
 * @property {number} p - pressure 0..1 (0.5 when unknown)
 */

/**
 * @typedef {Object} BrushPaintParams
 * @property {PaintPoint[]} points
 * @property {number} size native page px, a diameter
 * @property {string} color
 * @property {number} opacity
 * @property {number} flow
 * @property {number} hardness
 * @property {number} spacing
 * @property {boolean} pressureSize
 * @property {boolean} pressureOpacity
 * @property {number} seed
 */

/**
 * @typedef {Object} ClonePaintParams
 * @property {PaintPoint[]} points
 * @property {number} seed
 */

/**
 * @typedef {Object} ShapePaintParams
 * @property {{kind: string, points: Array<{x: number, y: number}>, feather: number}} shape
 *   the area to cover, in page percent with a page-pixel feather
 * @property {PaintPoint[]} points - the gesture, for provenance; may be empty
 * @property {string} color
 * @property {number} opacity
 * @property {number} flow
 * @property {number} hardness
 * @property {number} seed
 */

/**
 * Convert raw gesture points to normalized paint points with pressure.
 *
 * @param {Array<{x: number, y: number, p?: number}>} points
 * @returns {PaintPoint[]}
 */
export function paintPointsOf(points) {
  return (points ?? [])
    .filter((point) => point && Number.isFinite(point.x) && Number.isFinite(point.y))
    .map((point) => ({
      x: Math.round(point.x * 100) / 100,
      y: Math.round(point.y * 100) / 100,
      p: typeof point.p === 'number' && point.p > 0 ? Math.round(point.p * 1000) / 1000 : 0.5,
    }))
}

/**
 * Compute a deterministic unsigned 32-bit integer seed from point data.
 *
 * @param {Array<{x: number, y: number, p?: number}>} points
 * @returns {number}
 */
export function computeSeed(points) {
  if (!points || points.length === 0) return 0
  const first = points[0]
  const p = typeof first.p === 'number' && first.p > 0 ? first.p : 0.5
  const str = `${first.x}:${first.y}:${p}:${points.length}`
  let hash = 0
  for (let i = 0; i < str.length; i++) {
    hash = (Math.imul(31, hash) + str.charCodeAt(i)) | 0
  }
  return hash >>> 0
}

/**
 * Builds `params.paint` for brush (paint mode), the clone stamp, and a Shapes
 * gesture set to a solid colour. Returns null for all other tools/modes.
 *
 * @param {string} tool
 * @param {Record<string, unknown>} params
 * @param {Array<{x: number, y: number, p?: number}>} points
 * @returns {BrushPaintParams | ClonePaintParams | ShapePaintParams | null}
 */
export function paintParamsOf(tool, params = {}, points = []) {
  const pts = paintPointsOf(points)

  // **A shape filled with a flat colour is a paint, not a clean.** It takes
  // the same branch of the backend that the Brush's paint mode takes
  // (`region.rs#paint_plan`), so it skips the fit, the ladder and the quality
  // metric - there is nothing to assess about a colour somebody chose. What
  // differs is the coverage: a brush covers what its dabs swept, a shape
  // covers its own outline, which is why `shape` rides here instead of a
  // dab-spacing brush. `hardness` and `flow` are pinned at 100 because a
  // shape has no soft rim and no build-up: its edge is `feather`, which is
  // geometry and travels with the shape.
  if (tool === 'shapes' && params.mode === 'solid' && params.painted) {
    return {
      shape: params.painted,
      points: pts,
      color: String(params.color ?? '#000000'),
      opacity: Math.max(0, Math.min(100, Number(params.opacity ?? 100))),
      flow: 100,
      hardness: 100,
      seed: computeSeed(points),
    }
  }

  if (pts.length === 0) return null

  if (tool === 'brush' && params.mode === 'paint') {
    return {
      points: pts,
      size: Math.max(1, Number(params.size ?? 28)),
      color: String(params.color ?? '#000000'),
      opacity: Math.max(0, Math.min(100, Number(params.opacity ?? 100))),
      flow: Math.max(0, Math.min(100, Number(params.flow ?? 100))),
      hardness: Math.max(0, Math.min(100, Number(params.hardness ?? 70))),
      spacing: Math.max(1, Math.min(50, Number(params.spacing ?? 12))),
      pressureSize: true,
      pressureOpacity: false,
      seed: computeSeed(points),
    }
  }

  if (tool === 'cloneStamp') {
    return {
      points: pts,
      seed: computeSeed(points),
    }
  }

  return null
}
