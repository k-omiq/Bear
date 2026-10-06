<script>
  import { detectionMaskUrl } from '../api/tile.js'
  import { isHighlighted } from '../state/editor.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { maskColorFor } from '../model/masks.js'
  import { hexToRgb } from '../ui/color.js'
  import { backingScale, loadDetectionMask, paintMask } from './detectionmasks.svelte.js'

  /**
   * One detected region's mask, drawn where it sits on the page.
   *
   * **Inside the app** the mask arrives over the tile protocol as a white
   * shape cropped to its own bounds, with the bounds in a header
   * (`detectionmasks.svelte.js`). It is coloured into a canvas placed over
   * those bounds in page percent, grown by `MARGIN` CSS pixels on every side
   * so the outline has room outside the shape. The backing store follows the
   * canvas's drawn size times the display's pixel ratio, measured by a
   * `ResizeObserver`, so the outline stays about 1.5 CSS pixels at every zoom;
   * the size is read from `clientWidth` rather than the observer's entry,
   * because older WebKit hands `borderBoxSize` over in a different shape.
   *
   * **Outside it** - the mock in a plain browser, and the tests - there is no
   * protocol and no pixels, so the region's box stands in, drawn in the same
   * fill and outline. A mask that fails to load or to draw falls back to the
   * same stand-in rather than to nothing: the detection is still there and
   * will still be cleaned.
   *
   * Lit - under the pointer, selected, or lit from its Layers row - the
   * outline thickens and the fill strengthens, so the one the pointer is on
   * stands out of a page full of them.
   *
   * @type {{
   *   page: import('../api/backend.js').ApiPage,
   *   region: import('../api/backend.js').ApiRegion,
   * }}
   */
  let { page, region } = $props()

  /** CSS pixels of room around the image, enough for the lit outline. */
  const MARGIN = 3
  /** The outline, in CSS pixels, at rest and lit. */
  const OUTLINE = 1.5
  const OUTLINE_LIT = 2.5
  /** How many percentage points a lit mask's fill gains. */
  const LIT_BOOST = 15

  const url = $derived(detectionMaskUrl(page, region))
  const lit = $derived(isHighlighted(region.id))
  // Speech bubble text in one colour, text outside bubbles in the other; a
  // detection whose place is unknown takes the speech bubble one.
  const color = $derived(maskColorFor(region, session))
  const fill = $derived(
    Math.max(0, Math.min(100, Number(session.maskOpacity ?? 35) + (lit ? LIT_BOOST : 0))) / 100,
  )
  const pageWidth = $derived(Number(page.width) || 0)
  const pageHeight = $derived(Number(page.height) || 0)

  /** @type {import('./detectionmasks.svelte.js').MaskImage|null} */
  let loaded = $state.raw(null)
  let failed = $state(false)
  /** @type {HTMLCanvasElement|undefined} */
  let canvasEl = $state()
  /** The canvas as drawn, in CSS pixels. */
  let drawn = $state({ width: 0, height: 0 })

  const standin = $derived(!url || failed || !(pageWidth > 0) || !(pageHeight > 0))

  /** Where the image sits, in page percent. */
  const box = $derived(
    loaded && pageWidth > 0 && pageHeight > 0
      ? {
          x: (loaded.bounds.x / pageWidth) * 100,
          y: (loaded.bounds.y / pageHeight) * 100,
          w: (loaded.bounds.w / pageWidth) * 100,
          h: (loaded.bounds.h / pageHeight) * 100,
        }
      : null,
  )

  /** The stand-in's fill: the colour at the fill's strength. */
  const wash = $derived.by(() => {
    const rgb = hexToRgb(color)
    return rgb ? `rgba(${rgb.r}, ${rgb.g}, ${rgb.b}, ${fill})` : 'transparent'
  })

  // Fetch the mask whenever its URL moves, which an edit of this mask does
  // and an edit of any other does not. The picture already drawn stays until
  // the new one lands, so an edit does not blink. A page that leaves the
  // longstrip's window takes its request with it.
  $effect(() => {
    const target = url
    failed = false
    if (!target) {
      loaded = null
      return
    }
    const controller = typeof AbortController === 'function' ? new AbortController() : null
    let alive = true
    loadDetectionMask(target, controller ? { signal: controller.signal } : {}).then(
      (entry) => {
        if (alive) loaded = entry
      },
      (error) => {
        if (!alive || error?.name === 'AbortError') return
        console.warn('a detection mask could not be loaded', error)
        loaded = null
        failed = true
      },
    )
    return () => {
      alive = false
      controller?.abort()
    }
  })

  // The canvas's drawn size. Zoom changes it, and so does the sheet.
  $effect(() => {
    const node = canvasEl
    if (!node) return
    const measure = () => {
      drawn = { width: node.clientWidth, height: node.clientHeight }
    }
    measure()
    if (typeof ResizeObserver !== 'function') return
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    return () => observer.disconnect()
  })

  // Colour it. Redrawn when the mask, the size, the colour, the opacity or
  // the lit state moves. A canvas the engine will not give a context to - one
  // WebKit refuses as too large, or jsdom's - would leave nothing on the page
  // for a region that will still be cleaned, so it falls back to the stand-in.
  $effect(() => {
    const node = canvasEl
    const entry = loaded
    const { width, height } = drawn
    const style = { color, fill, outline: lit ? OUTLINE_LIT : OUTLINE }
    if (!node || !entry || !(width > 0) || !(height > 0)) return
    const scale = backingScale(width, height)
    const backingWidth = Math.max(1, Math.round(width * scale))
    const backingHeight = Math.max(1, Math.round(height * scale))
    if (node.width !== backingWidth) node.width = backingWidth
    if (node.height !== backingHeight) node.height = backingHeight
    const painted = paintMask(node, entry.image, {
      inset: MARGIN * scale,
      outline: style.outline * scale,
      color: style.color,
      fill: style.fill,
    })
    if (!painted) {
      console.warn('a detection mask could not be drawn', region.id)
      failed = true
    }
  })
</script>

{#if standin}
  <div
    class="standin"
    class:lit
    data-detection={region.id}
    data-mask="standin"
    style:left="{region.bbox.x}%"
    style:top="{region.bbox.y}%"
    style:width="{region.bbox.w}%"
    style:height="{region.bbox.h}%"
    style:background-color={wash}
    style:--mask-color={color}
  ></div>
{:else if box}
  <canvas
    bind:this={canvasEl}
    class="mask"
    class:lit
    data-detection={region.id}
    data-mask="image"
    style:left="calc({box.x}% - {MARGIN}px)"
    style:top="calc({box.y}% - {MARGIN}px)"
    style:width="calc({box.w}% + {MARGIN * 2}px)"
    style:height="calc({box.h}% + {MARGIN * 2}px)"
  ></canvas>
{/if}

<style>
  .standin,
  .mask {
    position: absolute;
    display: block;
    pointer-events: none;
  }

  /* The box a real mask would be drawn in, with the same outline just outside
     it: a spread shadow sits outside the border box and takes no layout. */
  .standin {
    border-radius: 3px;
    box-shadow: 0 0 0 1.5px var(--mask-color);
    transition: box-shadow var(--dur-fast) var(--ease);
  }
  .standin.lit { box-shadow: 0 0 0 2.5px var(--mask-color) }
</style>
