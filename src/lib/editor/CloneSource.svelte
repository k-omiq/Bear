<script>
  import { renderCloneTiles } from './paintrender.js'
  import { cleanedTiles } from './paintpreview.js'

  /**
   * What the clone stamp is about to copy, shown where it will land.
   *
   * **The source, inside the brush.** While the stamp has a source, the round
   * cursor is filled with the pixels the next dab would lay down: the page as
   * it stands, read from the source and shown at the pointer. Aiming a clone
   * is lining those pixels up with the ones around them, and that cannot be
   * done blind. It is drawn from the tiles already on the sheet - a
   * `drawImage` of each, shifted by the distance from brush to source - so it
   * follows the pointer with no round trip, and it shows the cleaned page
   * because that is what the stamp reads (`paint/mod.rs#render_clone`).
   *
   * **A cross on the source itself**, so the place being read from is marked
   * on the page as well as shown in the brush. It moves with the pointer once
   * a stroke has fixed the offset, and sits on the sampled point until then.
   *
   * **A target while the sample modifier is down**, in place of both: the
   * next click picks a source rather than painting, and the cursor says so
   * before the click.
   *
   * @type {{
   *   at: {x: number, y: number}|null,
   *   radius: {rx: number, ry: number},
   *   source: {x: number, y: number}|null,
   *   sampling?: boolean,
   *   version?: string,
   * }}
   * `at` is the brush and `source` is where a brush there reads from, both in
   * page percent; `radius` is the brush's, per axis. `version` is the cleaned
   * page's, so the overlay is redrawn when the pixels under it change.
   */
  let { at, radius, source, sampling = false, version = '' } = $props()

  /** @type {HTMLCanvasElement|undefined} */
  let canvas = $state()
  /** Counts tile loads: a tile that arrives late has to reach the overlay. */
  let loads = $state(0)

  const filled = $derived(!!at && !!source && !sampling && radius.rx > 0 && radius.ry > 0)

  $effect(() => {
    const onLoad = () => (loads += 1)
    document.addEventListener('paint-tile-loaded', onLoad)
    return () => document.removeEventListener('paint-tile-loaded', onLoad)
  })

  $effect(() => {
    loads
    version
    if (filled && canvas && at && source) draw(canvas, source, radius)
  })

  /**
   * Fill the overlay with the sheet as seen around `from`.
   *
   * The canvas is the brush's own box, at the screen's pixel density. Each
   * tile is placed where it lies on the sheet and the whole sheet is then
   * shifted so that `from` falls on the middle of the canvas.
   *
   * @param {HTMLCanvasElement} target
   * @param {{x: number, y: number}} from
   * @param {{rx: number, ry: number}} reach
   */
  function draw(target, from, reach) {
    const sheet = target.closest('.sheet')
    const box = sheet?.getBoundingClientRect()
    if (!sheet || !box || !(box.width > 0) || !(box.height > 0)) return
    const density = window.devicePixelRatio || 1
    const width = Math.max(1, Math.round(((reach.rx * 2) / 100) * box.width * density))
    const height = Math.max(1, Math.round(((reach.ry * 2) / 100) * box.height * density))
    if (target.width !== width) target.width = width
    if (target.height !== height) target.height = height
    const ctx = target.getContext('2d')
    if (!ctx) return
    ctx.clearRect(0, 0, width, height)

    renderCloneTiles(ctx, cleanedTiles(sheet), {
      width: box.width * density,
      height: box.height * density,
      dx: -((from.x - reach.rx) / 100) * box.width * density,
      dy: -((from.y - reach.ry) / 100) * box.height * density,
    })
  }
</script>

{#if filled && at}
  <canvas
    class="overlay"
    bind:this={canvas}
    aria-hidden="true"
    style:left="{at.x - radius.rx}%"
    style:top="{at.y - radius.ry}%"
    style:width="{radius.rx * 2}%"
    style:height="{radius.ry * 2}%"
  ></canvas>
{/if}

{#if sampling && at}
  <span class="mark target" aria-hidden="true" style:left="{at.x}%" style:top="{at.y}%"></span>
{:else if source}
  <span class="mark" data-clone-source aria-hidden="true" style:left="{source.x}%" style:top="{source.y}%"></span>
{/if}

<style>
  /* Round, like the brush: the box is the brush's in page percent, and the
     sheet is drawn at the page's own aspect ratio, so it is a circle. */
  .overlay {
    position: absolute;
    z-index: var(--z-draw);
    border-radius: 50%;
    pointer-events: none;
  }

  /* A cross of two hairlines, a fixed size on screen at every zoom. The light
     halo is what keeps the mark colour legible over black ink. */
  .mark {
    position: absolute;
    width: 19px;
    height: 19px;
    transform: translate(-50%, -50%);
    pointer-events: none;
    background:
      linear-gradient(var(--page-mark), var(--page-mark)) center / 1px 100% no-repeat,
      linear-gradient(var(--page-mark), var(--page-mark)) center / 100% 1px no-repeat;
    filter: drop-shadow(0 0 1px var(--paper)) drop-shadow(0 0 1px var(--paper));
  }

  /* The same cross in a ring: this click samples. */
  .target {
    width: 25px;
    height: 25px;
    border: 1px solid var(--page-mark);
    border-radius: 50%;
  }
</style>
