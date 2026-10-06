<script>
  import { isDetected } from '../model/masks.js'
  import DetectionMask from './DetectionMask.svelte'

  /**
   * The detected regions of one page, drawn as the masks Clean will erase.
   *
   * After a Detect nothing on the page has changed yet, and the whole point
   * of the step is to look at what was found before anything is erased. A box
   * around each detection answered "where did it look"; this answers "which
   * pixels will go", which is the question that decides whether to Clean. So
   * each detection is its real mask - the SAM text mask with its padding, and
   * the lettering it covers - filled at the selection opacity and outlined in
   * the selection colour for its kind of text, speech bubble or outside
   * (Settings › General). The selection tool edits exactly this area.
   *
   * It sits over both artwork layers and under `RegionLayer`, whose buttons
   * are transparent and keep every hover, selection, menu and keyboard route;
   * this layer takes no pointer at all. Like the region outlines it is not
   * clipped by the wipe: a mask is a statement about the region, not about the
   * cleaned pixels, and a detection has no cleaned pixels yet.
   *
   * Hidden from assistive technology: each detection is already a named
   * button in `RegionLayer`, and a picture of its mask would announce it twice.
   *
   * @type {{ page: import('../api/backend.js').ApiPage }}
   */
  let { page } = $props()

  const detections = $derived((page.regions ?? []).filter(isDetected))
</script>

{#if detections.length}
  <div class="masks" aria-hidden="true" data-detection-masks={page.id}>
    {#each detections as region (region.id)}
      <DetectionMask {page} {region} />
    {/each}
  </div>
{/if}

<style>
  .masks {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
</style>
