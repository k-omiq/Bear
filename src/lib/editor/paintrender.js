/**
 * How a planned dab becomes pixels on a `<canvas>` - and nothing else.
 *
 * **Pure, and deliberately DOM-free.** Everything here takes a 2D context as an
 * argument and never reaches for one, never touches `document`, and never
 * schedules a frame. That is what lets the whole of the live preview's drawing
 * be tested in the frontend's `node` test environment against a recording fake
 * context, in a repo whose vitest has no canvas at all - the component in
 * `PaintLayer.svelte` is then only lifecycle and sizing.
 *
 * ## The three coordinate spaces, and the one number that joins them
 *
 * A dab is planned once, in **native page pixels**
 * (`paintplan.js`), because that is the space the commit stamps in and parity
 * of the plan is the whole bargain. The canvas is a *proxy*:
 * whatever CSS box the sheet happens to occupy, times `devicePixelRatio`. So
 * every routine here takes a single `scale` = canvasWidth / pageWidth and
 * multiplies. Nothing else converts anything.
 *
 * ## Flow, opacity, and why there are two canvases
 *
 * `flow` is *per dab* and `opacity` is a ceiling on the *stroke*: overlapping
 * dabs at 20% flow build towards the opacity, and they must never build past
 * it. One canvas cannot express that - each dab would composite over the last
 * and a slow stroke would go solid. So the caller stamps into an offscreen
 * stroke canvas at per-dab alpha and then presents that canvas once, at
 * `globalAlpha = opacity/100`. [`presentStroke`] is that second step.
 *
 * This approximates rather than reproduces `cleaner-core`'s compositor, which
 * accumulates coverage in f32 and blends once. The falloff curve is matched
 * exactly ([`dabStops`] mirrors `paint/brush.rs#analytic_radial_coverage`); the
 * accumulation is not, because Canvas2D has no premultiplied coverage buffer to
 * borrow. §4 permits this: the preview owes the user the *plan*, not the bytes.
 */

/**
 * The smallest dab the preview will draw, in **canvas** pixels.
 *
 * 00-design.md risk 4: a native-space radius that is honest can still be a
 * quarter of a proxy pixel at a zoomed-out longstrip, and a stroke of
 * quarter-pixel discs is an invisible stroke - the user would see nothing at
 * all until the commit came back. A floor makes a thin stroke *too thick* in
 * the preview instead of absent, which is the failure that can be reasoned
 * about.
 */
export const MIN_PREVIEW_RADIUS = 0.75

/**
 * The radial falloff of one dab, as canvas gradient stops.
 *
 * Mirrors `crates/cleaner-core/src/paint/brush.rs#analytic_radial_coverage`:
 * full coverage inside `hardness` of the radius, then the shell falls off
 * along the same two-piece line that function uses - 1.0 at the core's edge,
 * 0.55 at 55% of the way out, 0 at the rim. A gradient with those three stops
 * is piecewise linear between them, which is exactly what the Rust does.
 *
 * @param {number} hardness - `0..100`
 * @returns {Array<[number, number]>} `[offset 0..1, coverage 0..1]`, in order
 */
export function dabStops(hardness) {
  const h = Math.min(Math.max(Number(hardness) / 100 || 0, 0), 1)
  if (h >= 1) return [[0, 1], [1, 1]]
  const shell = 1 - h
  return [
    [0, 1],
    [h, 1],
    [h + 0.55 * shell, 0.55],
    [1, 0],
  ]
}

/**
 * `#rrggbb` (or `#rgb`) and an alpha, as an `rgba()` string.
 *
 * Canvas gradients need a colour *per stop*, and the only way to vary alpha
 * across a gradient is to vary it in the colour. Anything unparseable falls
 * back to black rather than throwing: a preview that draws the wrong colour is
 * recoverable, one that throws inside a rAF is not.
 *
 * @param {string} hex
 * @param {number} alpha - `0..1`
 * @returns {string}
 */
export function rgba(hex, alpha) {
  const a = Math.min(Math.max(Number(alpha) || 0, 0), 1)
  const text = String(hex ?? '').trim()
  const short = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/i.exec(text)
  const long = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(text)
  let r = 0
  let g = 0
  let b = 0
  if (long) {
    r = parseInt(long[1], 16)
    g = parseInt(long[2], 16)
    b = parseInt(long[3], 16)
  } else if (short) {
    r = parseInt(short[1] + short[1], 16)
    g = parseInt(short[2] + short[2], 16)
    b = parseInt(short[3] + short[3], 16)
  }
  return `rgba(${r}, ${g}, ${b}, ${Math.round(a * 1000) / 1000})`
}

/**
 * One dab's radius in canvas pixels, floored so it stays visible.
 *
 * @param {{radius: number}} dab
 * @param {number} scale
 * @param {number} [floor]
 * @returns {number}
 */
export function previewRadius(dab, scale, floor = MIN_PREVIEW_RADIUS) {
  const r = Number(dab?.radius) * Number(scale)
  return Math.max(Number.isFinite(r) ? r : 0, floor)
}

/**
 * Stamp a slice of a planned stroke into a context, in colour.
 *
 * `from` is the count of dabs already drawn: the planner's output is
 * prefix-stable (`paintplan.js#planStroke`), so a live stroke redraws nothing
 * and each frame costs only the dabs that frame added.
 *
 * @param {CanvasRenderingContext2D} ctx
 * @param {import('./paintplan.js').PlannedDab[]} dabs
 * @param {{scale: number, color: string, hardness: number, from?: number, minRadius?: number}} style
 * @returns {number} how many dabs of the list have now been drawn
 */
export function renderDabs(ctx, dabs, style) {
  const list = dabs ?? []
  const from = Math.max(0, Math.min(Number(style?.from ?? 0), list.length))
  const scale = Number(style?.scale) || 0
  const color = String(style?.color ?? '#000000')
  const stops = dabStops(style?.hardness ?? 100)
  const floor = style?.minRadius ?? MIN_PREVIEW_RADIUS
  if (scale <= 0) return list.length

  for (let index = from; index < list.length; index += 1) {
    const dab = list[index]
    const alpha = Math.min(Math.max(Number(dab?.alpha) || 0, 0), 1)
    if (alpha <= 0) continue
    const x = Number(dab.x) * scale
    const y = Number(dab.y) * scale
    const radius = previewRadius(dab, scale, floor)
    if (!Number.isFinite(x) || !Number.isFinite(y) || radius <= 0) continue

    ctx.save()
    if (stops.length === 2) {
      // A hard tip has no gradient to build: one flat disc is both faster and
      // exactly what the Rust's `h >= 1` branch produces.
      ctx.globalAlpha = alpha
      ctx.fillStyle = color
    } else {
      const gradient = ctx.createRadialGradient(x, y, 0, x, y, radius)
      for (const [offset, coverage] of stops) gradient.addColorStop(offset, rgba(color, alpha * coverage))
      ctx.fillStyle = gradient
    }
    ctx.beginPath()
    ctx.arc(x, y, radius, 0, Math.PI * 2)
    ctx.fill()
    ctx.restore()
  }
  return list.length
}

/**
 * The union of a stroke's dab discs, as a path to clip with.
 *
 * Clone / heal's preview is not a colour: it is the page itself, read from
 * somewhere else and shown through the shape the brush has swept. That shape
 * is this path. `Path2D` is a browser global, so the caller passes the
 * constructor in - which is also what makes this testable.
 *
 * @param {new () => Path2D} Path
 * @param {import('./paintplan.js').PlannedDab[]} dabs
 * @param {{scale: number, from?: number, minRadius?: number}} style
 * @returns {Path2D}
 */
export function dabPath(Path, dabs, style) {
  const path = new Path()
  const list = dabs ?? []
  const scale = Number(style?.scale) || 0
  const floor = style?.minRadius ?? MIN_PREVIEW_RADIUS
  if (scale <= 0) return path
  for (let index = Math.max(0, Number(style?.from ?? 0)); index < list.length; index += 1) {
    const dab = list[index]
    const x = Number(dab?.x) * scale
    const y = Number(dab?.y) * scale
    const radius = previewRadius(dab, scale, floor)
    if (!Number.isFinite(x) || !Number.isFinite(y) || radius <= 0) continue
    path.moveTo(x + radius, y)
    path.arc(x, y, radius, 0, Math.PI * 2)
  }
  return path
}

/**
 * Draw the page's own proxy tiles into a context, offset by the clone vector.
 *
 * The seam's convention is `cloneOffset = source − strokeStart`, so the source
 * pixel for a target `t` is `t + offset` - and a canvas shows image pixel
 * `T − translate` at `T`, which makes the translate the *negative* of the
 * offset. That sign is the whole of this function's arithmetic and the one
 * thing worth reading twice.
 *
 * Each tile is an `<img>` or a patch layer's canvas already in the document,
 * positioned over its own share of the sheet in percent (`PageArtwork.svelte`),
 * so its destination rectangle is that share of the canvas. They are drawn in
 * order, each at its own `alpha` - a layer's CSS opacity, which `drawImage`
 * does not see. `drawImage` needs no readback, so a `tile://` image tainting
 * the canvas is irrelevant.
 *
 * @param {CanvasRenderingContext2D} ctx
 * @param {Array<{image: CanvasImageSource, left: number, top: number, width: number, height: number, alpha?: number}>} tiles
 *   `left`/`top`/`width`/`height` in **fractions of the canvas**, `0..1`; `alpha` `0..1`, opaque by default
 * @param {{width: number, height: number, dx: number, dy: number}} placement
 *   canvas size in px, and the *translate* in canvas px (already negated)
 * @returns {number} how many tiles were drawn
 */
export function renderCloneTiles(ctx, tiles, placement) {
  const width = Number(placement?.width) || 0
  const height = Number(placement?.height) || 0
  if (width <= 0 || height <= 0) return 0
  let drawn = 0
  const base = ctx.globalAlpha
  for (const tile of tiles ?? []) {
    if (!tile?.image) continue
    const w = tile.width * width
    const h = tile.height * height
    if (!(w > 0) || !(h > 0)) continue
    const alpha = Number(tile.alpha ?? 1)
    ctx.globalAlpha = base * (Number.isFinite(alpha) ? Math.min(Math.max(alpha, 0), 1) : 1)
    ctx.drawImage(
      tile.image,
      tile.left * width + placement.dx,
      tile.top * height + placement.dy,
      w,
      h,
    )
    drawn += 1
  }
  ctx.globalAlpha = base
  return drawn
}

/**
 * Show the accumulated stroke canvas on the visible one, at the stroke's
 * opacity ceiling.
 *
 * The visible canvas is cleared and redrawn whole every frame - one
 * `drawImage` of a canvas the same size, which is a blit - because the ceiling
 * applies to the *stroke*, not to what has been added since the last frame.
 *
 * @param {CanvasRenderingContext2D} ctx
 * @param {CanvasImageSource} stroke
 * @param {{width: number, height: number, opacity: number}} present - `opacity` is `0..100`
 */
export function presentStroke(ctx, stroke, present) {
  const width = Number(present?.width) || 0
  const height = Number(present?.height) || 0
  ctx.clearRect(0, 0, width, height)
  if (width <= 0 || height <= 0) return
  const opacity = Math.min(Math.max(Number(present?.opacity ?? 100), 0), 100) / 100
  if (opacity <= 0) return
  ctx.save()
  ctx.globalAlpha = opacity
  ctx.drawImage(stroke, 0, 0)
  ctx.restore()
}
