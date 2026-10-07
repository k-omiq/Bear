<script>
  import { untrack } from 'svelte'
  import { draft } from './draft.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { getBackend } from '../api/backend.js'
  import { pageVersion } from '../api/tile.js'
  import { paintParamsOf } from './paint.js'
  import { previewQueue, registerPaintPreview } from './paintpreview.js'

  /**
   * The pixels a paint or clone stroke is laying down, before the page's own
   * tiles show them.
   *
   * **Two kinds of image.** `live` is the stroke being drawn, rendered by the
   * native commit renderer and replaced as the pointer moves. When that stroke
   * is committed its last frame is **held**: kept on screen until the tiles
   * under it have loaded a version that includes it, so the paint never blinks
   * out between the gesture ending and the page catching up.
   *
   * **Held frames outlive the next stroke.** A clone stamp is used in quick
   * taps, and each tap is a commit of its own. So a new stroke starting does
   * not clear what is held: every held frame waits for the commits in flight
   * (`busy`) to land and for its own tiles to load. A stroke that was
   * abandoned is never held - the draft itself says which it was
   * (`draft.svelte.js#markCommitted`).
   *
   * `busy` is how many of this page's gestures are still being committed, and
   * `dropped` counts the commits that changed nothing: their frames will never
   * become tiles.
   *
   * @type {{
   *   page: import('../api/backend.js').ApiPage,
   *   busy?: number,
   *   dropped?: number,
   * }}
   */
  let { page, busy = 0, dropped = 0 } = $props()

  /** @typedef {{url: string, x: number, y: number, w: number, h: number, revision: string}} Frame */

  let root = $state()
  /** @type {Frame|null} */
  let live = $state(null)
  /** @type {Array<Frame & {initial: Map<string|undefined, string|undefined>}>} */
  let held = $state.raw([])
  let previousDraft = null
  let lastKey = ''
  let seenDropped = 0

  const active = $derived(draft.active?.pageId === page.id ? draft.active : null)
  const version = $derived(pageVersion(page, 'cleaned'))
  const pageId = $derived(page.id)
  const drawing = $derived(active?.kind === 'stroke' && (active.tool === 'cloneStamp' ||
    (active.tool === 'brush' && editor.toolParams.brush?.mode === 'paint')))

  /** Forget the stroke being drawn, and any frame of it still on its way. */
  function dropLive() {
    queue.cancel()
    if (live) URL.revokeObjectURL(live.url)
    live = null
    lastKey = ''
  }

  /** The stroke was committed: keep its last frame until the tiles show it. */
  function hold() {
    queue.cancel()
    lastKey = ''
    if (!live) return
    const initial = new Map()
    for (const tile of tilesUnder(live)) initial.set(tile.dataset.paintKey, tile.dataset.version)
    held = [...held, { ...live, initial }]
    live = null
  }

  function dropHeld() {
    for (const frame of held) URL.revokeObjectURL(frame.url)
    held = []
  }

  const queue = previewQueue(spec => getBackend().previewPaint(spec), result => {
    if (result.revision !== version || result.chapterId !== editor.chapter?.id || result.pageIndex !== page.index) return
    if (live) URL.revokeObjectURL(live.url)
    live = {
      url: URL.createObjectURL(new Blob([new Uint8Array(result.png)], { type: 'image/png' })),
      ...result.bounds,
      revision: result.revision,
    }
  }, dropLive)

  function submit() {
    if (!drawing || !active || !editor.chapter) return
    const params = { ...(editor.toolParams[active.tool] ?? {}) }
    if (active.tool === 'cloneStamp') {
      // Where the stroke reads from was fixed when it started, and the commit
      // sends the same two values (`drawing.svelte.js#cloneInto`).
      if (!active.clone) return
      params.cloneSource = active.clone.source
      params.cloneOffset = active.clone.offset
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
    pageId
    const onLoad = () => release()
    document.addEventListener('paint-tile-loaded', onLoad)
    return () => {
      document.removeEventListener('paint-tile-loaded', onLoad)
      previousDraft = null
      dropLive()
      dropHeld()
    }
  })

  // Capture the final pointer position before commit drops the draft. This is
  // the same paintParamsOf payload used by the commit seam, including its seed.
  $effect(() => registerPaintPreview(page.id, async () => { submit(); await queue.flush() }))
  $effect(() => {
    // Read all request dependencies here; lifecycle changes precede submission
    // so starting a stroke cannot cancel its own first frame.
    const current = drawing ? active : null
    const key = current ? JSON.stringify([current.points, current.clone,
      editor.toolParams[current.tool], version]) : ''
    untrack(() => {
      if (current !== previousDraft) {
        if (previousDraft?.committed) hold()
        else dropLive()
        previousDraft = current
      }
      if (key) submit()
    })
  })

  // A commit that changed nothing has no tiles coming, so its frame goes.
  // While another commit is in flight the frames stay: that one's tiles
  // release them all, and if it changes nothing either this runs again.
  $effect(() => {
    const count = dropped
    const idle = busy === 0
    untrack(() => {
      if (count === seenDropped) return
      seenDropped = count
      if (idle) dropHeld()
    })
  })

  /**
   * The cleaned tiles a frame lies over. On a longstrip a stroke can reach
   * into the page above or below, so the whole stage is searched.
   *
   * @param {{x: number, y: number, w: number, h: number}} frame
   * @returns {HTMLElement[]}
   */
  function tilesUnder(frame) {
    const sheet = root?.getBoundingClientRect()
    const scope = root?.closest('.stage') ?? root?.closest('.sheet')
    if (!sheet || !scope) return []
    const width = sheet.right - sheet.left
    const height = sheet.bottom - sheet.top
    const box = {
      left: sheet.left + (frame.x / 100) * width,
      top: sheet.top + (frame.y / 100) * height,
      right: sheet.left + ((frame.x + frame.w) / 100) * width,
      bottom: sheet.top + ((frame.y + frame.h) / 100) * height,
    }
    return [...scope.querySelectorAll('[data-artwork="cleaned"] img[data-version]')].filter(tile => {
      const r = tile.getBoundingClientRect()
      return r.right > box.left && r.left < box.right && r.bottom > box.top && r.top < box.bottom
    })
  }

  /** Let go of every held frame whose tiles now show what it showed. */
  function release() {
    if (held.length === 0 || busy > 0) return
    const kept = held.filter(frame => {
      if (version === frame.revision) return true
      const landed = tilesUnder(frame).every(tile =>
        tile.dataset.loadedVersion === tile.dataset.version &&
        frame.initial.get(tile.dataset.paintKey) !== tile.dataset.version)
      if (landed) URL.revokeObjectURL(frame.url)
      return !landed
    })
    if (kept.length !== held.length) held = kept
  }
  $effect(() => { version; busy; held; root; untrack(release) })
</script>

<div class="paints" bind:this={root} aria-hidden="true">
  {#each held as frame (frame.url)}
    <img class="paint" src={frame.url} alt=""
      style:left="{frame.x}%" style:top="{frame.y}%" style:width="{frame.w}%" style:height="{frame.h}%" />
  {/each}
  {#if live}
    <img class="paint" src={live.url} alt=""
      style:left="{live.x}%" style:top="{live.y}%" style:width="{live.w}%" style:height="{live.h}%" />
  {/if}
</div>

<style>
  .paints {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }

  .paint {
    position: absolute;
    /* The authoritative crop replaces these pixels, including transparency. */
    background: var(--paper);
  }
</style>
