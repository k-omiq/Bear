<script>
  import { untrack } from 'svelte'
  import { draft } from './draft.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { getBackend } from '../api/backend.js'
  import { pageVersion } from '../api/tile.js'
  import { cloneOffset } from './gesture.js'
  import { paintParamsOf } from './paint.js'
  import { previewQueue, registerPaintPreview } from './paintpreview.js'

  let { page, committing = false } = $props()
  let layer = $state()
  let image = $state(null)
  let heldVersion = $state(null)
  let previousDraft = null
  let previewRevision = null
  let lastKey = ''
  let url = null
  let initialVersions = new Map()
  const active = $derived(draft.active?.pageId === page.id ? draft.active : null)
  const version = $derived(pageVersion(page, 'cleaned'))
  const pageId = $derived(page.id)
  const drawing = $derived(active?.kind === 'stroke' && (active.tool === 'cloneHeal' ||
    (active.tool === 'brush' && editor.toolParams.brush?.mode === 'paint')))

  function clear() {
    queue.cancel()
    if (url) URL.revokeObjectURL(url)
    url = null
    image = null
    heldVersion = null
    previewRevision = null
    lastKey = ''
    initialVersions.clear()
  }
  const queue = previewQueue(spec => getBackend().previewPaint(spec), result => {
    if (result.revision !== version || result.chapterId !== editor.chapter?.id || result.pageIndex !== page.index) return
    if (url) URL.revokeObjectURL(url)
    url = URL.createObjectURL(new Blob([new Uint8Array(result.png)], { type: 'image/png' }))
    previewRevision = result.revision
    image = { url, ...result.bounds }
  }, clear)

  function submit() {
    if (!drawing || !active || !editor.chapter) return
    const params = { ...(editor.toolParams[active.tool] ?? {}) }
    if (active.tool === 'cloneHeal') {
      const resolved = cloneOffset({
        source: draft.cloneSource?.pageId === page.id ? draft.cloneSource : null,
        strokeStart: active.points[0], alignment: String(params.alignment ?? 'aligned'), offset: draft.cloneOffset,
      })
      if (!resolved) return
      params.cloneSource = resolved.source
      params.cloneOffset = resolved.offset
    }
    params.previewBbox = active.bbox
    params.paint = paintParamsOf(active.tool, params, active.points)
    const spec = { chapterId: editor.chapter.id, pageIndex: page.index,
      ...(Number.isInteger(page.sourceIndex) && page.sourceSha ? { sourceIndex: page.sourceIndex, sourceSha: page.sourceSha } : {}), tool: active.tool, params, revision: version }
    const key = JSON.stringify(spec)
    if (key === lastKey) return
    lastKey = key
    queue.submit(spec)
  }

  $effect(() => {
    const id = pageId
    untrack(clear)
    const onLoad = () => releaseWhenLoaded()
    document.addEventListener('paint-tile-loaded', onLoad)
    return () => { document.removeEventListener('paint-tile-loaded', onLoad); clear() }
  })

  // Capture the final pointer position before commit drops the draft. This is
  // the same paintParamsOf payload used by the commit seam, including its seed.
  $effect(() => registerPaintPreview(page.id, async () => { submit(); await queue.flush() }))
  $effect(() => {
    // Read all request dependencies here; lifecycle changes precede submission
    // so starting a stroke cannot cancel its own first frame.
    const current = drawing ? active : null
    const key = current ? JSON.stringify([current.points, editor.toolParams[current.tool],
      draft.cloneSource, draft.cloneOffset, version]) : ''
    const isCommitting = committing
    untrack(() => {
      if (current !== previousDraft) {
        if (current) clear()
        else if (previousDraft && isCommitting) heldVersion = previewRevision
        else clear()
        previousDraft = current
      }
      if (!current && !isCommitting) clear()
      if (key) submit()
    })
  })

  function releaseWhenLoaded() {
    if (!image) return
    const sheet = layer?.closest('.stage') ?? layer?.closest('.sheet')
    const tiles = [...(sheet?.querySelectorAll('[data-artwork="cleaned"] img[data-version]') ?? [])]
    const box = layer?.getBoundingClientRect()
    if (!box) return
    const affected = tiles.filter(tile => {
      const r = tile.getBoundingClientRect()
      return r.right > box.left && r.left < box.right && r.bottom > box.top && r.top < box.bottom
    })
    if (heldVersion === null) {
      for (const tile of affected) {
        const key = tile.dataset.paintKey
        if (key && !initialVersions.has(key)) initialVersions.set(key, tile.dataset.version)
      }
      return
    }
    if (version === heldVersion) return
    if (affected.length && affected.every(tile => tile.dataset.version &&
        tile.dataset.loadedVersion === tile.dataset.version &&
        (!initialVersions.has(tile.dataset.paintKey) || initialVersions.get(tile.dataset.paintKey) !== tile.dataset.version))) clear()
  }
  $effect(() => { version; image; heldVersion; layer; untrack(releaseWhenLoaded) })

</script>

{#if image}
  <img bind:this={layer} class="paint" src={image.url} alt="" aria-hidden="true"
    style:left="{image.x}%" style:top="{image.y}%" style:width="{image.w}%" style:height="{image.h}%" />
{/if}
<style>
  .paint {
    position: absolute;
    pointer-events: none;
    /* The authoritative crop replaces these pixels, including transparency. */
    background: var(--paper);
  }
</style>
