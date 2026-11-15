<script>
  import { tick } from 'svelte'

  // `full` is the page at its own resolution, laid over the proxy tiles past
  // the proxy's scale (`PageArtwork`). It is not a tile: it carries no
  // version or paint key, so nothing that waits for a page's tiles waits for
  // it. It says instead which version it is (`data-full-version`) and which
  // it shows (`data-full-loaded`): a paint preview is held until the page
  // under it at full resolution has the stroke (`PaintLayer#release`).
  // `proxy` marks a tile whose pixels are scaled down from the image's.
  let { tile, onloaded, full = false } = $props()
  let displayed = $state(null)

  // Keep the decoded DOM node itself. Recreating even the same URL can leave
  // a blank frame while WebKit prepares the new image for painting.
  const images = $derived(displayed && displayed.src !== tile.src ? [displayed, tile] : [tile])

  async function loaded(event, requested) {
    const image = event.currentTarget
    try {
      await image.decode?.()
    } catch {
      // A failed replacement must leave the last decoded pixels on screen.
      return
    }
    if (requested.version !== tile.version || requested.src !== tile.src) return
    displayed = { src: requested.src, version: requested.version }
    // Readiness and paint-preview consumers inspect the updated DOM attributes.
    await tick()
    onloaded()
  }
</script>

{#each images as requested (requested.src)}
  {@const current = requested.src === tile.src}
  <img class="scan" class:held={!current} class:full src={requested.src}
    data-version={current && !full ? tile.version : undefined}
    data-loaded-version={!full && displayed?.src === requested.src ? displayed.version : undefined}
    data-paint-key={current && !full ? tile.paintKey : undefined}
    data-proxy={tile.proxy ? '' : undefined}
    data-full-version={current && full ? tile.version : undefined}
    data-full-loaded={full && displayed?.src === requested.src ? displayed.version : undefined}
    alt="" decoding="async" draggable="false"
    style:visibility={current && displayed && displayed.src !== requested.src ? 'hidden' : null}
    onload={event => loaded(event, requested)}
    style:top={tile.top} style:left={tile.left} style:width={tile.width} style:height={tile.height} />
{/each}

<style>
  .scan { position: absolute; display: block; }
</style>
