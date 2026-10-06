<script>
  import { tick } from 'svelte'

  let { tile, onloaded } = $props()
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
  <img class="scan" class:held={!current} src={requested.src}
    data-version={current ? tile.version : undefined}
    data-loaded-version={displayed?.src === requested.src ? displayed.version : undefined}
    data-paint-key={current ? tile.paintKey : undefined}
    alt="" decoding="async" draggable="false"
    style:visibility={current && displayed && displayed.src !== requested.src ? 'hidden' : null}
    onload={event => loaded(event, requested)}
    style:top={tile.top} style:left={tile.left} style:width={tile.width} style:height={tile.height} />
{/each}

<style>
  .scan { position: absolute; display: block; }
</style>
