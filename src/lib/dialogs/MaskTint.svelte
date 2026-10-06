<script>
  /**
   * A one-bit raster drawn over the page as a flat tint, in source pixels.
   *
   * The raster is a grayscale PNG (the SAM evidence mask, or a prepared write
   * support W). A pixel is in the set when it is not black, which is the
   * backend's own rule for these rasters. Each tile's canvas is exactly as
   * many pixels as the source region it covers, and CSS scales it with
   * `image-rendering: pixelated`, so at 1:1 one canvas pixel is one source
   * pixel and at any zoom the edge is the raster's edge, never a smoothed one.
   *
   * This replaces CSS luminance masking (`mask-image` with `mask-mode`),
   * which the macOS 11 WebKit the app ships on does not support unprefixed:
   * there the tint drew as a solid block, or not at all. Canvas 2D is
   * supported everywhere the app runs.
   *
   * A raster whose size is not exactly `bounds` is refused and nothing is
   * drawn: a stretched mask would show pixels that are not in the set.
   * Tiles keep each canvas within WebKit's canvas size limits.
   */
  import { untrack } from 'svelte'

  /**
   * @type {{
   *   src: string | null | undefined,
   *   bounds: { x: number, y: number, w: number, h: number },
   *   pageWidth: number,
   *   pageHeight: number,
   *   tone: 'evidence' | 'write',
   * }}
   */
  let { src, bounds, pageWidth, pageHeight, tone } = $props()

  /** Source pixels per canvas side. 2048² is 16 MB of pixel data per tile. */
  const TILE = 2048

  const tiles = $derived(tilesOf(bounds))
  /** @type {HTMLCanvasElement[]} */
  let canvases = $state([])
  /** 'loading' until the raster is decoded, then 'drawn' or 'refused'. */
  let drawState = $state('loading')

  /** @param {{x: number, y: number, w: number, h: number}} box */
  function tilesOf(box) {
    const list = []
    if (!box || !(box.w > 0) || !(box.h > 0)) return list
    for (let y = 0; y < box.h; y += TILE) {
      for (let x = 0; x < box.w; x += TILE) {
        list.push({ x, y, w: Math.min(TILE, box.w - x), h: Math.min(TILE, box.h - y) })
      }
    }
    return list
  }

  /** @param {number} value @param {number} total */
  function percent(value, total) { return `${value / total * 100}%` }

  /**
   * Draw one tile: the raster's pixels 1:1, thresholded to full alpha where
   * the raster is not black, then filled with the tone's color.
   *
   * @param {HTMLCanvasElement} canvas
   * @param {CanvasImageSource} image
   * @param {{x: number, y: number, w: number, h: number}} tile - in raster pixels
   */
  function paintTile(canvas, image, tile) {
    const context = canvas?.getContext?.('2d')
    if (!context || typeof context.drawImage !== 'function' || typeof context.getImageData !== 'function') return false
    canvas.width = tile.w
    canvas.height = tile.h
    context.imageSmoothingEnabled = false
    context.clearRect?.(0, 0, tile.w, tile.h)
    context.drawImage(image, tile.x, tile.y, tile.w, tile.h, 0, 0, tile.w, tile.h)
    const pixels = context.getImageData(0, 0, tile.w, tile.h)
    const data = pixels.data
    for (let at = 0; at < data.length; at += 4) {
      const on = data[at + 3] > 0 && (data[at] > 0 || data[at + 1] > 0 || data[at + 2] > 0)
      data[at] = 0
      data[at + 1] = 0
      data[at + 2] = 0
      data[at + 3] = on ? 255 : 0
    }
    context.putImageData(pixels, 0, 0)
    context.globalCompositeOperation = 'source-in'
    context.fillStyle = getComputedStyle(canvas).color
    context.fillRect(0, 0, tile.w, tile.h)
    context.globalCompositeOperation = 'source-over'
    return true
  }

  $effect(() => {
    const url = src
    const box = bounds
    const list = tiles
    void canvases.length
    drawState = 'loading'
    if (!url || !list.length || typeof Image === 'undefined') return
    let alive = true
    const image = new Image()
    image.onload = () => {
      if (!alive) return
      if (image.naturalWidth !== box.w || image.naturalHeight !== box.h) {
        drawState = 'refused'
        for (const canvas of untrack(() => canvases)) {
          if (canvas) { canvas.width = 0; canvas.height = 0 }
        }
        return
      }
      const targets = untrack(() => canvases)
      let painted = true
      list.forEach((tile, index) => { painted = paintTile(targets[index], image, tile) && painted })
      drawState = painted ? 'drawn' : 'refused'
    }
    image.onerror = () => { if (alive) drawState = 'refused' }
    image.src = url
    return () => {
      alive = false
      image.onload = null
      image.onerror = null
    }
  })
</script>

{#each tiles as tile, index (`${tile.x}:${tile.y}`)}
  <canvas
    class="tint {tone}"
    data-tint={tone}
    data-state={drawState}
    aria-hidden="true"
    bind:this={canvases[index]}
    style:left={percent(bounds.x + tile.x, pageWidth)}
    style:top={percent(bounds.y + tile.y, pageHeight)}
    style:width={percent(tile.w, pageWidth)}
    style:height={percent(tile.h, pageHeight)}
  ></canvas>
{/each}

<style>
  .tint {
    position: absolute;
    display: block;
    image-rendering: pixelated;
    pointer-events: none;
  }
  /* Fallbacks match WorkflowAnalysis's tokens, for a tint drawn outside it. */
  .tint.evidence { color: var(--wa-evidence, rgba(2, 132, 199, .26)) }
  .tint.write { color: var(--wa-write, rgba(255, 72, 26, .72)) }
</style>
