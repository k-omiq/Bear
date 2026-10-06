<script>
  import { untrack } from 'svelte'
  import { cachedLayer, loadLayer, noteLayerDrawn } from './patchlayers.svelte.js'

  /**
   * One patch's layer: a canvas placed over its bounds on the page's proxy
   * grid, drawn from the decoded image, at the layer's opacity.
   *
   * **The old picture stays until the new one is drawn.** When the URL moves,
   * the canvas is left alone while the new image is fetched and decoded; then
   * its size, its place and its pixels change together, in one task, before
   * the next paint. An `<img>` given a new `src` would blank in WebKit until
   * the new bytes decoded, and the page's lettering would show through.
   *
   * Placed and sized by writing the canvas's own style in the same step as
   * its pixels, rather than through reactive style bindings, so a moved patch
   * never shows its new pixels at its old place for a frame.
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

  /**
   * Put a decoded layer on the canvas, or clear it for a `204`.
   *
   * @param {import('../api/boundedimage.js').BoundedImage} entry
   */
  function draw(entry) {
    const node = canvasEl
    if (!node) return
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
      node.getContext('2d')?.drawImage(image, 0, 0)
    }
    noteLayerDrawn(pageId)
    onsettle(layer.id)
  }

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
  style:opacity={layer.opacity}
></canvas>

<style>
  .layer {
    position: absolute;
    display: none;
    pointer-events: none;
  }
</style>
