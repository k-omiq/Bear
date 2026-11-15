<script>
  import { SvelteSet } from 'svelte/reactivity'
  import { proxyPlan } from '../api/tile.js'
  import { editor, pages } from '../state/editor.svelte.js'
  import PatchLayer from './PatchLayer.svelte'
  import { pageLayers } from './patchlayers.svelte.js'

  /**
   * The cleaned side of a page: one canvas per patch, bottom first, over the
   * `source` tiles `PageArtwork` draws below (`patchlayers.svelte.js`).
   *
   * `ready` is the id of the page once it can be shown - its regions are in
   * hand and every layer it has has drawn once, or failed - and `null` until
   * then. An id rather than a flag, because a paginated sheet is handed the
   * next page in place, and a flag left over from the last page would show
   * the new one on its lettering. `PageSheet` holds the page's artwork back
   * until it names the page it is showing.
   *
   * `from`/`to` are the share of the page, in percent of its height, that is
   * loaded; a patch of the page's own outside it is not mounted. A part
   * reaching across a join is always mounted - there are a few, and where it
   * lands is the protocol's answer, not this side's guess.
   *
   * `hide` names a layer drawn but not shown: the one the held gradient's
   * preview stands in for.
   *
   * `full` draws each layer on the page's own grid rather than its proxy's,
   * for a page shown past its proxy's scale: an eraser stroke then takes off
   * the page's own pixels, as the commit does, not proxy pixels stretched.
   *
   * @type {{
   *   page: import('../api/backend.js').ApiPage,
   *   from?: number,
   *   to?: number,
   *   hide?: string|null,
   *   full?: boolean,
   *   ready?: string|null,
   * }}
   */
  let { page, from = 0, to = 100, hide = null, full = false, ready = $bindable(null) } = $props()

  const plan = $derived(full ? { width: Number(page.width) || 0, height: Number(page.height) || 0 } : proxyPlan(page))
  const list = $derived(editor.project?.mode === 'longstrip' ? pages() : [page])
  const layers = $derived(
    pageLayers(page, list, { full }).filter(
      (layer) => !layer.own || (layer.bbox.y <= to && layer.bbox.y + layer.bbox.h >= from),
    ),
  )

  /** The layers that have drawn once, or failed, on this page. */
  const settled = new SvelteSet()
  let settledFor = /** @type {string|null} */ (null)

  // A new page in the same sheet starts with nothing drawn.
  $effect.pre(() => {
    if (page.id === settledFor) return
    settledFor = page.id
    settled.clear()
  })

  /** @param {string} id */
  function settle(id) {
    settled.add(id)
  }

  $effect(() => {
    ready = page.resident !== false && layers.every((layer) => settled.has(layer.id)) ? page.id : null
  })
</script>

{#each layers as layer (layer.id)}
  <PatchLayer {layer} {plan} pageId={page.id} hidden={layer.id === hide} onsettle={settle} />
{/each}
