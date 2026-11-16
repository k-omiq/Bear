<script>
  import { gridAxis, gridCovers } from './pixelgrid.js'

  /**
   * The pixel grid: one line at the left and the top of every image pixel, a
   * device pixel wide. Each line is placed by itself (`pixelgrid.js` says
   * why a repeated CSS background cannot do this).
   *
   * Two canvases over the part of the sheet on screen, and nothing larger.
   * One is a single row of device pixels stretched down the view: it holds
   * the vertical lines. The other is a single column stretched across: the
   * horizontal lines. So the grid costs a few thousand pixels of backing at
   * any zoom, and a redraw is a few hundred one-pixel fills.
   *
   * The canvases sit on the sheet and scroll with it. They are drawn a
   * little past the viewport, and drawn again only when a scroll gets near
   * their edge or the sheet changes size.
   */

  /** @type {{pixel: number, columns: number, rows: number}} */
  let { pixel, columns, rows } = $props()

  /**
   * The grey is the image's, not the interface's: it has to show on black
   * ink and on white paper alike, in either theme, which no token is for.
   */
  const LINE = 'rgba(128, 128, 128, .45)'

  let root = $state(/** @type {HTMLElement|undefined} */ (undefined))
  let across = $state(/** @type {HTMLCanvasElement|undefined} */ (undefined))
  let down = $state(/** @type {HTMLCanvasElement|undefined} */ (undefined))

  /** What the canvases hold now. Not reactive: only `draw` reads it. */
  let held = /** @type {{key: string, x: {from: number, to: number}, y: {from: number, to: number}}|null} */ (null)

  /** @param {number} value its fraction, in 64ths, which is as fine as layout goes */
  const phase = (value) => Math.round((value - Math.floor(value)) * 64)

  function hide() {
    held = null
    for (const canvas of [across, down]) if (canvas) canvas.style.display = 'none'
  }

  /**
   * @param {HTMLCanvasElement} canvas
   * @param {number[]} lines
   * @param {number} length
   * @param {boolean} vertical the lines are vertical, so the canvas is one row
   */
  function fill(canvas, lines, length, vertical) {
    const width = vertical ? length : 1
    const height = vertical ? 1 : length
    if (canvas.width !== width) canvas.width = width
    if (canvas.height !== height) canvas.height = height
    const ctx = canvas.getContext('2d')
    if (!ctx) return
    ctx.clearRect(0, 0, width, height)
    ctx.fillStyle = LINE
    for (const at of lines) ctx.fillRect(vertical ? at : 0, vertical ? 0 : at, 1, 1)
  }

  function draw() {
    if (!root || !across || !down) return
    const dpr = globalThis.devicePixelRatio || 1
    const box = root.getBoundingClientRect()
    const x = gridAxis({ edge: box.left, pixel, count: columns, view: globalThis.innerWidth, dpr })
    const y = gridAxis({ edge: box.top, pixel, count: rows, view: globalThis.innerHeight, dpr })
    if (!x || !y) return hide()
    const left = box.left * dpr
    const top = box.top * dpr
    // A whole-pixel scroll keeps every line where it is on the sheet. Any
    // other change (a zoom, a half-pixel move, another screen) does not.
    const key = `${pixel}|${columns}|${rows}|${dpr}|${phase(left)}|${phase(top)}`
    if (held?.key === key && gridCovers(held.x, x, left) && gridCovers(held.y, y, top)) return
    fill(across, x.lines, x.length, true)
    fill(down, y.lines, y.length, false)
    for (const canvas of [across, down]) {
      canvas.style.display = ''
      canvas.style.left = `${x.start / dpr - box.left}px`
      canvas.style.top = `${y.start / dpr - box.top}px`
      canvas.style.width = `${x.length / dpr}px`
      canvas.style.height = `${y.length / dpr}px`
    }
    held = {
      key,
      x: { from: x.start - left, to: x.start + x.length - left },
      y: { from: y.start - top, to: y.start + y.length - top },
    }
  }

  // A zoom changes `pixel` and the sheet's size together, so the sheet is
  // measured after the DOM has both. Scrolls are heard on the document, in
  // the capture phase: the sheet does not know which ancestor scrolls it.
  $effect(() => {
    void pixel, void columns, void rows
    if (!root) return
    draw()
    const observer = globalThis.ResizeObserver ? new ResizeObserver(draw) : null
    observer?.observe(root)
    document.addEventListener('scroll', draw, { capture: true, passive: true })
    globalThis.addEventListener('resize', draw)
    return () => {
      observer?.disconnect()
      document.removeEventListener('scroll', draw, { capture: true })
      globalThis.removeEventListener('resize', draw)
    }
  })
</script>

<div class="pixel-grid" bind:this={root} aria-hidden="true">
  <canvas bind:this={across}></canvas>
  <canvas bind:this={down}></canvas>
</div>

<style>
  /* Over the masks, under the outlines. */
  .pixel-grid {
    position: absolute;
    inset: 0;
    z-index: var(--z-mask);
    pointer-events: none;
  }

  /* One row or one column of device pixels, stretched over the view. Never
     smoothed: a line is one device pixel or it is a blur. */
  canvas {
    position: absolute;
    image-rendering: pixelated;
  }
</style>
