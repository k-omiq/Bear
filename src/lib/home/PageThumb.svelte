<script>
  import { tileUrl } from '../api/tile.js'

  /**
   * One page as a thumbnail on the Pages screen: the first proxy tile of the
   * cleaned page over `tile://`, whole and centred rather than cropped, and
   * loaded only as it scrolls near, since a chapter can hold hundreds.
   *
   * In a plain browser and in tests `tileUrl` answers `null` and the page's
   * stand-in panels are drawn instead, as `CoverArt` does.
   *
   * @type {{ page: import('../api/backend.js').ApiPage }}
   */
  let { page } = $props()

  const src = $derived(tileUrl(page, 'cleaned', 0))
  const panels = $derived(/** @type {any} */ (page)?.panels ?? [])
  const seed = $derived(/** @type {any} */ (page)?.layout ?? 0)
</script>

<span class="art" aria-hidden="true">
  {#if src}
    <img class="page" {src} alt="" loading="lazy" decoding="async" draggable="false" />
  {:else}
    {#each panels as panel, i (i)}
      <span
        class="panel"
        class:alt={(seed + i) % 2 === 1}
        style:left="{panel.x}%"
        style:top="{panel.y}%"
        style:width="{panel.w}%"
        style:height="{panel.h}%"
      ></span>
    {/each}
  {/if}
</span>

<style>
  .art { position: absolute; inset: 0; display: block }
  .panel { position: absolute; display: block; background: var(--art2) }
  .panel.alt { background: var(--art) }
  .page {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    object-position: top;
  }
</style>
