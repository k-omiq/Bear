<script>
  import { tileOrigin, pageVersion } from '../api/tile.js'
  import { editor } from '../state/editor.svelte.js'
  import { pageRatio } from './zoom.js'
  import { isDrawingTool } from './tools.js'
  import { isRegionCopy } from '../model/layers.js'
  import DetectionMasks from './DetectionMasks.svelte'
  import DrawLayer from './DrawLayer.svelte'
  import PageArtwork from './PageArtwork.svelte'
  import PatchLayers from './PatchLayers.svelte'
  import RegionLayer from './RegionLayer.svelte'
  import { layerPreviews } from './layerpreview.svelte.js'

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
   *   waiting?: boolean,
   *   onshown?: (shown: boolean) => void,
   * }}
   */
  let {
    page, width, current = true, strip = false, tabbable = true, stripMinY = 0, stripMaxY = 100,
    // The share of the page, in percent of its height, whose pixels to load.
    loadFrom = 0, loadTo = 100,
    // A neighbour mounted ahead of a page turn is `waiting` while the page on
    // screen is still loading; `onshown` is how that page, and no other, says
    // whether it is shown yet.
    waiting = false, onshown,
  } = $props()

  const ratio = $derived(pageRatio(page))
  const wipe = $derived(editor.wipe)
  const original = $derived(editor.originalVisible)

  // `inset(top right bottom left)`: the cleaned page occupies everything left
  // of the divider, so the right inset is whatever the wipe has not reached.
  const clip = $derived(original ? 'inset(0 100% 0 0)' : `inset(0 ${100 - wipe}% 0 0)`)
  const divider = $derived(!original && wipe < 100)

  // The stack the wipe leaves nothing of, when there is one: the cleaned page
  // while the original is shown outright or the wipe is at zero, the original
  // under a full wipe.
  const covered = $derived(original || wipe === 0 ? 'cleaned' : wipe === 100 ? 'original' : null)

  // The four drag tools put a drawing surface over the sheet. It covers the
  // region buttons, so `RegionLayer` yields the *pointer* while it is up - a
  // drag has to be able to start over a region, and erasing means starting
  // over one deliberately. The buttons keep their tab stops either way, and
  // the surface routes a tap straight back to the region under it, so the
  // region-click seam survives underneath.
  const copySelected = $derived((editor.chapter?.pages ?? []).some((candidate) =>
    candidate.regions?.some((region) => region.id === editor.selectionId && isRegionCopy(region))))
  const placingCopy = $derived(editor.tool === 'cloneStamp' && editor.toolParams.cloneStamp?.mode === 'region' && copySelected)
  const movingCopy = $derived((current || strip) && (editor.chapter?.pages ?? []).some((candidate) =>
    candidate.regions?.some(region => isRegionCopy(region) && layerPreviews.has(region.id))))
  const drawing = $derived(isDrawingTool(editor.tool) && !placingCopy)

  const layered = tileOrigin() !== null

  // Tiles are asked for in the order they are looked at. `tile.rs` draws three
  // pages at a time and a paginated chapter mounts three sheets in two
  // variants, so a jump to a cold page queued the page on screen behind the
  // one before it. A neighbour asks for nothing while it is `waiting`, and
  // every sheet asks for the stack the wipe covers one pass after the stack on
  // screen. Once asked for, a stack stays mounted: its decoded tiles are what
  // makes the next turn instant. A strip has its own window (`strip.js`) and
  // the mock has no tiles, so both mount everything at once.
  let begun = $state(false)
  const asked = $derived(!layered || !waiting || begun)
  const both = $derived(!layered || strip || begun)
  $effect(() => {
    if (asked) begun = true
  })

  // Inside the app a page is shown once the tiles of the view on screen have
  // decoded, so it never opens on its lettering and cleans itself a moment
  // later. Under the cleaned page those are the cleaned tiles, at any wipe.
  // With the original shown outright only the source tiles are seen, and the
  // page does not wait for cleaned ones nobody is looking at. Shown once, it
  // stays shown: later edits retain the old tiles while replacements decode,
  // and a cleaned stack still decoding when the original is let go covers the
  // lettering with its own paper. This includes neighbours mounted ahead of
  // navigation. A tile that never answers does not hold the page back past
  // `SHOW_AFTER_MS`, counted from when it was asked for. The mock has no
  // tiles and nothing to wait for.
  const SHOW_AFTER_MS = 2000
  let sourceReady = $state(/** @type {string|null} */ (null))
  let layersReady = $state(/** @type {string|null} */ (null))
  let shownFor = $state(/** @type {string|null} */ (null))
  let cleanedVersion = $state(/** @type {string|null} */ (null))
  let previewPage = $state(/** @type {string|null} */ (null))
  let previewShownFor = $state(/** @type {string|null} */ (null))
  let previewSourceReady = $state(/** @type {string|null} */ (null))
  let previewLayersReady = $state(/** @type {string|null} */ (null))
  const previewing = $derived(layered && previewPage === page.id)
  const previewReady = $derived(previewSourceReady === page.id && previewLayersReady === page.id)
  // Prepare independent canvases when a copy is selected. Show them during
  // manipulation and until the committed composite has decoded. At rest the
  // native composite preserves color-managed edge blending exactly.
  $effect(() => {
    if (copySelected && (current || strip)) previewPage = page.id
    else if (!movingCopy && cleanedVersion === pageVersion(page, 'cleaned')) previewPage = null
  })
  $effect(() => {
    if (movingCopy) previewShownFor = page.id
    else if (cleanedVersion === pageVersion(page, 'cleaned')) previewShownFor = null
  })
  const shown = $derived(!layered || shownFor === page.id)
  $effect(() => {
    const ready = covered === 'cleaned' ? sourceReady : layersReady
    if (ready !== null && ready === page.id) shownFor = page.id
  })
  $effect(() => {
    const id = page.id
    if (shown || !asked) return
    const timer = setTimeout(() => (shownFor = id), SHOW_AFTER_MS)
    return () => clearTimeout(timer)
  })
  $effect(() => {
    if (current) onshown?.(shown)
  })
</script>

<div
  class="sheet"
  class:strip
  style:width="{width}px"
  style:aspect-ratio="1 / {ratio}"
>
  {#if asked && (both || covered !== 'original')}
    <PageArtwork {page} variant="original" from={loadFrom} to={loadTo} hidden={!shown} bind:ready={sourceReady} />
  {/if}
  <div class="cleaned" style:clip-path={clip}>
    {#if asked && (both || covered !== 'cleaned')}
      <PageArtwork {page} variant="cleaned" from={loadFrom} to={loadTo} hidden={!shown} bind:ready={layersReady} bind:readyVersion={cleanedVersion} />
      {#if previewing}
        <div class="regional-artwork" style:visibility={shown && previewReady && previewShownFor === page.id ? null : 'hidden'}>
          <PageArtwork {page} variant="cleaned" composite={false} from={loadFrom} to={loadTo} bind:ready={previewSourceReady} />
          <PatchLayers {page} from={loadFrom} to={loadTo} bind:ready={previewLayersReady} />
        </div>
      {/if}
    {/if}
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

  .regional-artwork {
    position: absolute;
    inset: 0;
    overflow: hidden;
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
