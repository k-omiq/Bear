<script>
  import { tileOrigin } from '../api/tile.js'
  import { editor } from '../state/editor.svelte.js'
  import { pageRatio } from './zoom.js'
  import { isDrawingTool } from './tools.js'
  import DetectionMasks from './DetectionMasks.svelte'
  import DrawLayer from './DrawLayer.svelte'
  import PageArtwork from './PageArtwork.svelte'
  import RegionLayer from './RegionLayer.svelte'

  /**
   * One page: the `--paper` sheet, its shadow, its aspect ratio, and the stack
   * of layers on it. The sheet is **the one thing in the centre that floats**;
   * the field around it carries no shadow at all: the image is the brightest
   * thing on screen, and in dark the field must not glow against the artwork.
   *
   * The stack, bottom to top:
   *
   *   PageArtwork variant="original"   the source page
   *   PageArtwork variant="cleaned"    native composite, clipped by the wipe
   *   DetectionMasks                   what Clean will erase, per detection
   *   RegionLayer                      region outlines, markers, hit targets
   *   the wipe divider
   *
   * **The wipe is a clip, not a predicate.** Two images, one of them cut off
   * at `editor.wipe`, is how the real thing will work once `PageArtwork` is an
   * an image element - and it is why a region is not asked whether it is left or right
   * of the wipe. The prototype's `(r.x + r.w/2) <= wipe` test is a prototype
   * shortcut. Holding or pinning the original (`editor.originalVisible`) clips
   * the cleaned layer away entirely and suppresses the divider: the source is
   * shown outright, not wiped to zero.
   */

  /**
   * @type {{
   *   page: import('../api/backend.js').ApiPage,
   *   width: number,
   *   current?: boolean,
   *   strip?: boolean,
   *   tabbable?: boolean,
   *   stripMinY?: number,
   *   stripMaxY?: number,
   *   loadFrom?: number,
   *   loadTo?: number,
   * }}
   */
  let {
    page, width, current = true, strip = false, tabbable = true, stripMinY = 0, stripMaxY = 100,
    // The share of the page, in percent of its height, whose pixels to load.
    loadFrom = 0, loadTo = 100,
  } = $props()

  const ratio = $derived(pageRatio(page))
  const wipe = $derived(editor.wipe)
  const original = $derived(editor.originalVisible)

  // `inset(top right bottom left)`: the cleaned page occupies everything left
  // of the divider, so the right inset is whatever the wipe has not reached.
  const clip = $derived(original ? 'inset(0 100% 0 0)' : `inset(0 ${100 - wipe}% 0 0)`)
  const divider = $derived(!original && wipe < 100)

  // The four drag tools put a drawing surface over the sheet. It covers the
  // region buttons, so `RegionLayer` yields the *pointer* while it is up - a
  // drag has to be able to start over a region, and erasing means starting
  // over one deliberately. The buttons keep their tab stops either way, and
  // the surface routes a tap straight back to the region under it, so the
  // region-click seam survives underneath.
  const drawing = $derived(isDrawingTool(editor.tool))

  // Inside the app a page is shown once its cleaned tiles have decoded, so it
  // never opens on its lettering and cleans itself a moment later. Shown once,
  // it stays shown: later edits retain the old tiles while replacements decode.
  // This includes neighbours mounted ahead of navigation. A tile that never
  // answers does not hold the page back past `SHOW_AFTER_MS`. The mock has no
  // tiles and nothing to wait for.
  const SHOW_AFTER_MS = 2000
  const layered = tileOrigin() !== null
  let layersReady = $state(/** @type {string|null} */ (null))
  let shownFor = $state(/** @type {string|null} */ (null))
  const shown = $derived(!layered || shownFor === page.id)
  $effect(() => {
    if (layersReady !== null && layersReady === page.id) shownFor = page.id
  })
  $effect(() => {
    const id = page.id
    if (shown) return
    const timer = setTimeout(() => (shownFor = id), SHOW_AFTER_MS)
    return () => clearTimeout(timer)
  })
</script>

<div
  class="sheet"
  class:strip
  style:width="{width}px"
  style:aspect-ratio="1 / {ratio}"
>
  <PageArtwork {page} variant="original" from={loadFrom} to={loadTo} hidden={!shown} />
  <div class="cleaned" style:clip-path={clip}>
    <PageArtwork {page} variant="cleaned" from={loadFrom} to={loadTo} hidden={!shown} bind:ready={layersReady} />
  </div>

  <!-- Not clipped by the wipe, for the reason the region outlines are not:
       a detection has no cleaned pixels yet, and its mask is the plan. -->
  {#if current || strip}
    <DetectionMasks {page} />

    <RegionLayer {page} {tabbable} interactive={!drawing} {stripMinY} {stripMaxY} />

    {#if drawing}
      <DrawLayer {page} {tabbable} {strip} {stripMinY} {stripMaxY} />
    {/if}
  {/if}

  {#if divider}
    <div class="divider" style:left="{wipe}%" aria-hidden="true"></div>
  {/if}
</div>

<style>
  .sheet {
    position: relative;
    flex: none;
    background: var(--paper);
    box-shadow: var(--page-shadow);
    /* Lets everything drawn on the page size itself from the page's own width
       (`cqw`) instead of taking the zoom as a prop - which is what keeps
       `PageArtwork` swappable for an image element. */
    container-type: inline-size;
  }

  /* Paginated sheets float above the canvas. A longstrip is one continuous
     image: shadows and focus outlines would draw artificial seams. */
  .sheet.strip {
    box-shadow: none;
  }

  /* A native preview may extend across a longstrip join. Container queries
     make sheets stacking contexts, so lift its anchor above the next sheet
     until the affected committed tiles have replaced the preview. */
  .sheet:has(:global(.paint)) {
    z-index: 1;
  }

  .cleaned {
    position: absolute;
    inset: 0;
  }

  .divider {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--page-divider);
    box-shadow: 0 0 0 1px var(--page-halo);
    pointer-events: none;
  }
</style>
