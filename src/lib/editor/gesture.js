/**
 * Gesture-to-geometry arithmetic: turning what a pointer (or an arrow key) did
 * into a region's bounding box.
 *
 * Pure - no Svelte, no DOM, no state. Everything here works in the region's
 * **own normalised coordinate space**: `bbox` is `{x, y, w, h}` as percentages
 * of the page, which is what `ApiRegion.bbox` already is, what `RegionLayer`
 * draws with and what the export will read. Keeping the gesture in that space
 * is what makes the canvas, the Layers panel and the export describe the same
 * rectangle.
 *
 * **The scale a pointer is mapped through is the drawn one.** `pointIn` takes
 * the sheet's own measured rectangle rather than a zoom factor: the sheet is
 * laid out at `displayZoom()` already, so its box *is* the drawn scale, and a
 * measurement cannot drift from it the way a second computation of it could.
 */

/** The page's coordinate span - a bbox is a percentage of the page. */
export const PAGE_SPAN = 100

/**
 * The smallest mask a gesture may produce, in page percent. A zero-area mask
 * is not a mask; on a 1600px page this is 8px, which is the region of
 * `min_mask_thickness`.
 */
export const MIN_SPAN = 0.5

/** Below this much pointer movement (CSS px) a gesture is a tap, not a drag. */
export const TAP_SLOP = 3

/** @typedef {{x: number, y: number, p?: number}} Point */
/** @typedef {{x: number, y: number, w: number, h: number}} Bbox */
/** @typedef {{left: number, top: number, width: number, height: number}} Rect */

/**
 * @param {number} value
 * @param {number} min
 * @param {number} max
 * @returns {number}
 */
function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value))
}

/**
 * Where a pointer is on the page, in page percent.
 *
 * @param {number} clientX
 * @param {number} clientY
 * @param {Rect} rect - the sheet's measured box, i.e. the drawn scale
 * @param {number} [pressure] - pointer pressure 0..1 (0.5 when unknown or zero)
 * @param {boolean} [clampToPage] - false for a continuous longstrip gesture
 * @returns {Point}
 */
export function pointIn(clientX, clientY, rect, pressure, clampToPage = true) {
  const width = rect.width || 1
  const height = rect.height || 1
  const p = typeof pressure === 'number' && pressure > 0 ? pressure : 0.5
  return {
    // A strip continues vertically. Horizontally it still has page edges (and
    // possibly gutters beside a narrower neighbour), so x always clamps.
    x: clamp(((clientX - rect.left) / width) * PAGE_SPAN, 0, PAGE_SPAN),
    y: clampToPage
      ? clamp(((clientY - rect.top) / height) * PAGE_SPAN, 0, PAGE_SPAN)
      : ((clientY - rect.top) / height) * PAGE_SPAN,
    p,
  }
}

/** A drag box that may cross a longstrip page boundary. */
export function stripRectBetween(a, b) {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    w: Math.max(MIN_SPAN, Math.abs(a.x - b.x)),
    h: Math.max(MIN_SPAN, Math.abs(a.y - b.y)),
  }
}

/** Bounds of a stroke in anchor-page coordinates, without a page-edge clamp. */
export function stripBoundsOf(points, radius = {}) {
  if (!points || points.length === 0) return null
  const rx = radius.rx ?? 0
  const ry = radius.ry ?? 0
  const xs = points.map((point) => point.x)
  const ys = points.map((point) => point.y)
  const x = Math.min(...xs) - rx
  const y = Math.min(...ys) - ry
  return {
    x,
    y,
    w: Math.max(MIN_SPAN, Math.max(...xs) - Math.min(...xs) + rx * 2),
    h: Math.max(MIN_SPAN, Math.max(...ys) - Math.min(...ys) + ry * 2),
  }
}

/**
 * Bring a box onto the page and give it a floor. Order matters: the size is
 * floored first, then the origin is pulled back so the whole box fits.
 *
 * @param {Bbox} bbox
 * @returns {Bbox}
 */
export function clampBbox(bbox) {
  const w = clamp(bbox.w, MIN_SPAN, PAGE_SPAN)
  const h = clamp(bbox.h, MIN_SPAN, PAGE_SPAN)
  return {
    x: clamp(bbox.x, 0, PAGE_SPAN - w),
    y: clamp(bbox.y, 0, PAGE_SPAN - h),
    w,
    h,
  }
}

/**
 * The box two dragged corners describe, in either drag direction.
 *
 * @param {Point} a
 * @param {Point} b
 * @returns {Bbox}
 */
export function rectBetween(a, b) {
  return clampBbox({
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    w: Math.abs(a.x - b.x),
    h: Math.abs(a.y - b.y),
  })
}

/**
 * The box a run of points occupies, grown by a radius on each axis - which is
 * how a brush stroke becomes a mask: the stroke is the centre line, the mask is
 * the swept disc.
 *
 * @param {Point[]} points
 * @param {{rx?: number, ry?: number}} [radius] - in page percent, per axis
 * @returns {Bbox|null} null for an empty run
 */
export function boundsOf(points, radius = {}) {
  if (!points || points.length === 0) return null
  const rx = radius.rx ?? 0
  const ry = radius.ry ?? 0
  let minX = Infinity
  let minY = Infinity
  let maxX = -Infinity
  let maxY = -Infinity
  for (const point of points) {
    if (point.x < minX) minX = point.x
    if (point.x > maxX) maxX = point.x
    if (point.y < minY) minY = point.y
    if (point.y > maxY) maxY = point.y
  }
  return clampBbox({ x: minX - rx, y: minY - ry, w: maxX - minX + rx * 2, h: maxY - minY + ry * 2 })
}

/**
 * A brush's radius in page percent, per axis. The parameter is in **page
 * pixels** - a brush is a tool on the image, not on the screen, so a stroke
 * covers the same ink at every zoom - and the page is not square, so the two
 * axes differ.
 *
 * @param {number} sizePx - the `size` parameter, a diameter
 * @param {number} pageWidth - the page's own pixel width
 * @param {number} pageHeight
 * @returns {{rx: number, ry: number}}
 */
export function brushRadius(sizePx, pageWidth, pageHeight) {
  const radius = Math.max(0, Number(sizePx) || 0) / 2
  return {
    rx: (radius / (pageWidth || 1)) * PAGE_SPAN,
    ry: (radius / (pageHeight || 1)) * PAGE_SPAN,
  }
}

/**
 * Whether a brush should lay down another stamp here. `spacing` is a
 * percentage of the brush's own size, which is what every raster editor means
 * by it: a wide brush at 12% steps further than a narrow one.
 *
 * @param {Point|null} last - the last stamp, or null for the first
 * @param {Point} next
 * @param {{rx: number, ry: number}} radius
 * @param {number} spacing - the `spacing` parameter, 1..50
 * @returns {boolean}
 */
export function shouldStamp(last, next, radius, spacing) {
  if (!last) return true
  const step = (Math.max(radius.rx, radius.ry) * 2 * Math.max(1, spacing)) / PAGE_SPAN
  const dx = next.x - last.x
  const dy = next.y - last.y
  return Math.hypot(dx, dy) >= Math.max(step, 0.05)
}

/**
 * The region under a single point - a tap, not a drag. Later regions win, so
 * the hit matches what is drawn on top.
 *
 * @param {Point} point
 * @param {Array<{id: string, bbox: Bbox}>} regions
 * @returns {any|null}
 */
export function regionAt(point, regions) {
  let hit = null
  for (const region of regions ?? []) {
    const box = region?.bbox
    if (!box) continue
    if (
      point.x >= box.x &&
      point.x <= box.x + box.w &&
      point.y >= box.y &&
      point.y <= box.y + box.h
    ) {
      hit = region
    }
  }
  return hit
}

/**
 * Where a context menu should open for a `contextmenu` event.
 *
 * A pointer raises one at the pointer. The **keyboard** raises the same event
 * - `Shift`+`F10`, and the context-menu key - and reports no position at all,
 * so the anchor is then the middle of whatever the event was raised on. Both
 * routes have to exist: a menu only the mouse can
 * open is a set of actions only the mouse can reach.
 *
 * Duck-typed on the event and taking a measured rectangle, like `pointIn`, so
 * it is testable without a DOM.
 *
 * @param {{clientX?: number, clientY?: number}} event
 * @param {Rect|null} rect - the box of the element the event was raised on
 * @returns {{x: number, y: number}} in client pixels
 */
export function menuPoint(event, rect) {
  const x = Number(event?.clientX ?? 0)
  const y = Number(event?.clientY ?? 0)
  if (x > 0 || y > 0) return { x, y }
  if (!rect) return { x: 0, y: 0 }
  return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }
}

/**
 * How wide "here" is for a Detect of one area, in percent of the page's width:
 * 256 px on a 1600 px page, about one line of lettering either side of the
 * pointer. The native side reads a wider window around it for context
 * (`run.rs#DetectArea`); this is only what a found region has to touch.
 */
export const DETECT_AREA_SPAN = 16

/**
 * The area a Detect of one spot is held to: a box around `point`, square in
 * page pixels, moved back inside the page where the point is near an edge.
 *
 * @param {{x: number, y: number}} point - page percent
 * @param {number} pageWidth - page pixels
 * @param {number} pageHeight - page pixels
 * @returns {Bbox}
 */
export function detectAreaAround(point, pageWidth, pageHeight) {
  const w = DETECT_AREA_SPAN
  const h = Math.min(PAGE_SPAN, (w * pageWidth) / pageHeight)
  return {
    x: clamp(point.x - w / 2, 0, PAGE_SPAN - w),
    y: clamp(point.y - h / 2, 0, PAGE_SPAN - h),
    w,
    h,
  }
}

/**
 * The centre of a region's box in client pixels - `pointIn` run backwards,
 * through the same measured sheet rectangle.
 *
 * The keyboard's context menu over the drawing surface needs it: the surface
 * is one element covering the whole sheet, so the middle of *it* says nothing
 * about which region the menu is for, and the region the menu is for is the
 * selected one.
 *
 * @param {Bbox} bbox
 * @param {Rect} rect - the sheet's measured box, i.e. the drawn scale
 * @returns {{x: number, y: number}} in client pixels
 */
export function clientCentre(bbox, rect) {
  return {
    x: rect.left + ((bbox.x + bbox.w / 2) / PAGE_SPAN) * rect.width,
    y: rect.top + ((bbox.y + bbox.h / 2) / PAGE_SPAN) * rect.height,
  }
}

/**
 * Move a box, keeping it on the page.
 *
 * @param {Bbox} bbox
 * @param {number} dx
 * @param {number} dy
 * @returns {Bbox}
 */
export function moveBbox(bbox, dx, dy) {
  return clampBbox({ ...bbox, x: bbox.x + dx, y: bbox.y + dy })
}

/**
 * Grow or shrink a box from its top-left corner, keeping it on the page.
 *
 * @param {Bbox} bbox
 * @param {number} dw
 * @param {number} dh
 * @returns {Bbox}
 */
export function resizeBbox(bbox, dw, dh) {
  return clampBbox({ ...bbox, w: bbox.w + dw, h: bbox.h + dh })
}

/**
 * A box of a given size centred on the page - where a keyboard draft starts,
 * because a keyboard has no pointer to start it under.
 *
 * @param {number} w
 * @param {number} h
 * @returns {Bbox}
 */
export function centredBbox(w, h) {
  return clampBbox({ x: (PAGE_SPAN - w) / 2, y: (PAGE_SPAN - h) / 2, w, h })
}

/**
 * What a key means to the drawing surface.
 *
 * The keyboard route to a mask, as one table rather than as a branch tree in
 * the component: activate to open a draft or to commit the one that is open,
 * arrows to move it and Shift-arrows to resize it, `S` to sample the clone
 * source. `Escape` is deliberately absent - abandoning a gesture is the
 * editor's own `cancelInteraction`, and routing it here would give it two
 * owners.
 *
 * @param {{key: string, shiftKey?: boolean, metaKey?: boolean, ctrlKey?: boolean}} event
 * @param {{hasDraft: boolean, cloneCapable?: boolean, step?: number}} context
 * @returns {{kind: 'open'|'commit'|'sample'}|{kind: 'move'|'resize', dx: number, dy: number}|null}
 */
export function draftKeyIntent(event, context) {
  if (!event || event.metaKey || event.ctrlKey) return null
  const key = event.key
  if (key === 'Enter' || key === ' ') return { kind: context.hasDraft ? 'commit' : 'open' }
  if ((key === 's' || key === 'S') && context.cloneCapable) return { kind: 'sample' }
  if (!context.hasDraft) return null

  const step = context.step ?? 1
  const move = ARROWS[key]
  if (!move) return null
  return { kind: event.shiftKey ? 'resize' : 'move', dx: move[0] * step, dy: move[1] * step }
}

/** @type {Record<string, [number, number]>} */
const ARROWS = {
  ArrowLeft: [-1, 0],
  ArrowRight: [1, 0],
  ArrowUp: [0, -1],
  ArrowDown: [0, 1],
}

/**
 * Clone / heal: where the sampled pixels come from for this stroke.
 *
 * `aligned` keeps the offset between source and stroke **fixed** once it has
 * been established, so successive strokes read on from one another;
 * `nonAligned` re-anchors the source to the sampled point on every stroke, so
 * every stroke starts reading from the same place.
 *
 * @param {{source: Point|null, strokeStart: Point, alignment: string, offset: Point|null}} spec
 * @returns {{offset: Point, source: Point}|null} null when nothing has been sampled
 */
export function cloneOffset({ source, strokeStart, alignment, offset }) {
  if (!source) return null
  if (alignment === 'aligned' && offset) {
    return { offset, source: { x: strokeStart.x + offset.x, y: strokeStart.y + offset.y } }
  }
  const next = { x: source.x - strokeStart.x, y: source.y - strokeStart.y }
  return { offset: next, source }
}

/**
 * The AI mask brush's stroke width when its parameters carry none - the same
 * number its defaults start at. Lives here rather than in the component
 * because the preview, the cursor and the geometry that crosses the seam all
 * have to agree about how wide the stroke is, and three copies of a number
 * agree only by luck.
 */
export const AI_STROKE_PX = 36

/**
 * The stroke a round brush painted, in the shape the seam carries:
 * **the path and the radius**, not the
 * box around them.
 *
 * The two spaces are deliberately different and each is the one its side can
 * check. The points are page percent, like every other `bbox` on the seam, so
 * the canvas and the backend describe the same places. The radius is page
 * **pixels**, because a brush is a tool on the image rather than on the screen
 * - the same stroke covers the same ink at every zoom - and because a circle in
 * pixels is an ellipse in percent, so a single percentage radius could not
 * describe a round brush on a page that is not square.
 *
 * Nothing is rasterised here. `Mask::from_stroke` in
 * `crates/cleaner-core/src/mask.rs` sweeps the discs at the page's own
 * resolution, which is one rounding rather than two.
 *
 * @param {Point[]} points - the path, in page percent
 * @param {number} sizePx - the `size` parameter: a **diameter** in page pixels
 * @returns {{points: Point[], radius: number}|null} null when there is no path
 */
export function paintedStroke(points, sizePx) {
  const path = (points ?? []).filter(
    (point) => point && Number.isFinite(point.x) && Number.isFinite(point.y),
  )
  if (path.length === 0) return null
  return {
    points: path.map((point) => ({ x: round(point.x), y: round(point.y) })),
    // Half a pixel is the floor a stroke somebody drew cannot fall below: an
    // empty mask is not a mask, and the backend refuses one.
    radius: Math.max(0.5, (Math.max(0, Number(sizePx)) || 0) / 2),
  }
}

/**
 * The `points` attribute of an SVG polyline drawn in page-percent space.
 *
 * @param {Point[]} points
 * @returns {string}
 */
export function polylinePoints(points) {
  return (points ?? []).map((point) => `${round(point.x)},${round(point.y)}`).join(' ')
}

/**
 * @param {number} value
 * @returns {number} two decimal places - enough for a 1600px page, and it
 *   keeps the serialised path short during a drag
 */
function round(value) {
  return Math.round(value * 100) / 100
}
