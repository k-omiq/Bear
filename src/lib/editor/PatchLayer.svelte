<script>
  import { untrack } from 'svelte'
  import { cachedLayer, loadLayer, noteLayerDrawn } from './patchlayers.svelte.js'
  import { layerPreviews } from './layerpreview.svelte.js'
  import { drawErased, eraseMasks } from './erasepreview.svelte.js'

  /**
   * One patch's layer: a canvas placed over its bounds on the page's proxy
   * grid, drawn from the decoded image, at the layer's opacity.
   *
   * **The old picture stays until the new one is drawn.** When the URL moves,
   * the canvas is left alone while the new image is fetched and decoded; then
   * its size, its place and its pixels change together, in one task, before
   * the next paint. An image element given a new URL would blank in WebKit until
   * the new bytes decoded, and the page's lettering would show through.
   *
   * Placed and sized by writing the canvas's own style in the same step as
   * its pixels, rather than through reactive style bindings, so a moved patch
   * never shows its new pixels at its old place for a frame.
   *
   * A Brush layer is drawn with the eraser strokes on its page taken off
   * (`erasepreview.svelte.js`), each only off the picture it first reached.
   *
   * @type {{
   *   layer: import('./patchlayers.svelte.js').PageLayer,
   *   plan: {width: number, height: number},
   *   pageId: string,
   *   onsettle: (id: string) => void,
   * }}
   */
  let { layer, plan, pageId, onsettle } = $props()

  /** @type {HTMLCanvasElement|undefined} */
  let canvasEl = $state()
  let drawnStyle = $state({ offsetX: 0, offsetY: 0, rotation: 0 })
  const placement = $derived(layerPreviews.get(layer.id) ?? layer.style ?? drawnStyle)
  // In the sheet's own width (`--sheet-w`, set by `PageSheet`), so the move
  // follows the zoom without taking it as a prop.
  const sheetShare = (/** @type {number} */ px) => `calc(var(--sheet-w) * ${px / (layer.pageWidth || 1)})`
  /** The picture on the canvas, and the URL it came from. */
  let shown = /** @type {import('../api/boundedimage.js').BoundedImage|null} */ (null)
  let shownUrl = /** @type {string|null} */ (null)
  /** The picture each eraser stroke first reached, by stroke id. */
  const reached = new Map()
  const masks = $derived(layer.brush ? (eraseMasks.get(pageId) ?? []) : [])
  const movement = $derived(`translate(${sheetShare(placement.offsetX - drawnStyle.offsetX)}, ${sheetShare(placement.offsetY - drawnStyle.offsetY)}) rotate(${placement.rotation - drawnStyle.rotation}deg)`)

  /**
   * Put a decoded layer on the canvas, or clear it for a `204`.
   *
   * @param {import('../api/boundedimage.js').BoundedImage} entry
   */
  function draw(entry) {
    const node = canvasEl
    if (!node) return
    drawnStyle = { offsetX: 0, offsetY: 0, rotation: 0, ...(layer.style ?? {}) }
    const image = entry.image
    const bounds = entry.bounds
    if (!image || !bounds || !(plan.width > 0) || !(plan.height > 0)) {
      node.width = 0
      node.height = 0
      node.style.display = 'none'
    } else {
      node.width = bounds.w
      node.height = bounds.h
      node.style.left = `${(bounds.x / plan.width) * 100}%`
      node.style.top = `${(bounds.y / plan.height) * 100}%`
      node.style.width = `${(bounds.w / plan.width) * 100}%`
      node.style.height = `${(bounds.h / plan.height) * 100}%`
      node.style.display = 'block'
      const context = node.getContext('2d')
      if (context) drawErased(context, image, erasing(bounds), bounds, plan)
    }
    shown = entry
    shownUrl = layer.url
    noteLayerDrawn(pageId)
    onsettle(layer.id)
  }

  /** How many strokes the picture on the canvas has taken off it. */
  let erasedBy = 0

  /**
   * The eraser strokes to take off the layer's current picture: those over
   * it that first reached this picture.
   *
   * @param {{x: number, y: number, w: number, h: number}} bounds - the picture's, in proxy pixels
   */
  function erasing(bounds) {
    const live = new Set()
    const list = masks.filter((mask) => {
      live.add(mask.id)
      if (!reached.has(mask.id)) reached.set(mask.id, layer.url)
      const x = (mask.box.x / 100) * plan.width
      const y = (mask.box.y / 100) * plan.height
      const over = x < bounds.x + bounds.w && x + (mask.box.w / 100) * plan.width > bounds.x &&
        y < bounds.y + bounds.h && y + (mask.box.h / 100) * plan.height > bounds.y
      return over && reached.get(mask.id) === layer.url
    })
    for (const id of reached.keys()) if (!live.has(id)) reached.delete(id)
    erasedBy = list.length
    return list
  }

  // A stroke that grows or goes is drawn again, over the picture already
  // shown. While a newer picture is on its way the canvas is left as it is:
  // the stroke being let go must not bring back the paint it took off.
  $effect(() => {
    for (const mask of masks) mask.tick
    untrack(() => {
      const node = canvasEl
      const image = shown?.image
      const bounds = shown?.bounds
      if (!node || !image || !bounds || shownUrl !== layer.url) return
      const before = erasedBy
      const list = erasing(bounds)
      if (list.length === 0 && before === 0) return
      const context = node.getContext('2d')
      if (!context) return
      context.clearRect(0, 0, node.width, node.height)
      drawErased(context, image, list, bounds, plan)
    })
  })

  $effect(() => {
    const url = layer.url
    if (!canvasEl) return
    const hit = cachedLayer(url)
    if (hit) {
      // Drawing counts the page's draws, which this effect must not track.
      untrack(() => draw(hit))
      return
    }
    let alive = true
    loadLayer(url).then(
      (entry) => {
        if (alive) draw(entry)
      },
      (error) => {
        if (!alive) return
        console.warn('a patch layer could not be loaded', layer.id, error)
        onsettle(layer.id)
      },
    )
    return () => {
      alive = false
    }
  })
</script>

<canvas
  class="layer"
  bind:this={canvasEl}
  data-layer={layer.id}
  width="0"
  height="0"
  style:opacity={placement.opacity === undefined ? layer.opacity : placement.opacity / 100}
  style:transform={movement}
></canvas>

<style>
  .layer {
    position: absolute;
    display: none;
    pointer-events: none;
  }
</style>
