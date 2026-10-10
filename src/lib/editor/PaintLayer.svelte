<script>
  import { untrack } from 'svelte'
  import { draft } from './draft.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { getBackend } from '../api/backend.js'
  import { decodeImage } from '../api/boundedimage.js'
  import { pageVersion } from '../api/tile.js'
  import { paintParamsOf } from './paint.js'
  import { previewQueue, registerPaintPreview } from './paintpreview.js'

  /**
   * The pixels a paint or clone stroke is laying down, before the page's own
   * tiles show them.
   *
   * **Only the paint, on a canvas.** A frame is a see-through picture of the
   * painted pixels and nothing else, at the tiles' own pixel density. So
   * nothing in this layer has a background, and the art around the stroke is
   * never drawn a second time. A frame is decoded before its canvas is
   * touched; then the canvas's size, its place and its pixels change together,
   * in one task, as `PatchLayer.svelte` does it. An `<img>` given a new `src`
   * would blank in WebKit until the new bytes decoded, on every frame of the
   * stroke.
   *
   * **Two kinds of frame.** `live` is the stroke being drawn, rendered by the
   * native commit renderer and replaced as the pointer moves. When that stroke
   * is committed its last frame is **held**: kept on screen until the tiles
   * under it have loaded a version that includes it, so the paint never blinks
   * out between the gesture ending and the page catching up. A stroke has one
   * canvas from its first frame until it is let go, live or held.
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

  /**
   * `id` is the stroke's, and so its canvas's: every frame of one stroke
   * carries the same one. The box is in page percent.
   *
   * @typedef {{id: number, image: CanvasImageSource, x: number, y: number, w: number, h: number, revision: string}} Frame
   */

  let root = $state()
  /** @type {Frame|null} */
  let live = $state.raw(null)
  /** @type {Array<Frame & {initial: Map<string|undefined, string|undefined>}>} */
  let held = $state.raw([])
  /** One list, the live frame on top, so a frame keeps its canvas when it is held. */
  const frames = $derived(live ? [...held, live] : held)
  let strokes = 0
  /**
   * The picture of an answer the queue has been handed and has not presented.
   * The queue asks for one frame at a time, and it presents an answer in the
   * turn it gets it or drops it without a word (`paintpreview.js`). So a
   * picture still here when the next frame is asked for was dropped as stale.
   *
   * @type {CanvasImageSource|null}
   */
  let offered = null
  /**
   * How many times the frames on their way have been given up (`cancel`). A
   * picture that finishes decoding after that is one the queue will not
   * present, and nothing else is left to free it.
   */
  let cancels = 0
  let previousDraft = null
  let lastKey = ''
  let seenDropped = 0

  const active = $derived(draft.active?.pageId === page.id ? draft.active : null)
  const version = $derived(pageVersion(page, 'cleaned'))
  const pageId = $derived(page.id)
  const drawing = $derived(active?.kind === 'stroke' && (active.tool === 'cloneStamp' ||
    (active.tool === 'brush' && editor.toolParams.brush?.mode === 'paint')))

  /**
   * Give a decoded picture back. A bitmap keeps its pixels until it is closed;
   * the `Image` an older WebKit decodes into has nothing to close.
   *
   * @param {any} image
   */
  function free(image) {
    image?.close?.()
  }

  /** Give up every frame still on its way. */
  function cancel() {
    cancels += 1
    queue.cancel()
  }

  /** Forget the stroke being drawn, and any frame of it still on its way. */
  function dropLive() {
    cancel()
    free(offered)
    offered = null
    free(live?.image)
    live = null
    lastKey = ''
  }

  /** The stroke was committed: keep its last frame until the tiles show it. */
  function hold() {
    cancel()
    lastKey = ''
    if (!live) return
    const initial = new Map()
    for (const tile of tilesUnder(live)) initial.set(tile.dataset.paintKey, tile.dataset.version)
    held = [...held, { ...live, initial }]
    live = null
  }

  function dropHeld() {
    for (const frame of held) free(frame.image)
    held = []
  }

  /**
   * One frame from the native renderer, decoded. The decode is part of the
   * request so that it is the queue that waits on it: an answer that went
   * stale while it decoded is dropped by the queue's own checks, and a flush
   * does not return until the final frame is in `live`, where `hold` finds it.
   *
   * @param {Record<string, any>} spec
   */
  async function request(spec) {
    free(offered)
    offered = null
    const asked = cancels
    const result = await getBackend().previewPaint(spec)
    if (!result) return result
    const image = await decodeImage(new Blob([new Uint8Array(result.png)], { type: 'image/png' }))
    if (asked !== cancels) return free(image)
    offered = image
    return { ...result, image }
  }

  const queue = previewQueue(request, result => {
    offered = null
    if (result.revision !== version || result.chapterId !== editor.chapter?.id || result.pageIndex !== page.index) return free(result.image)
    free(live?.image)
    live = { id: live?.id ?? (strokes += 1), image: result.image, ...result.bounds, revision: result.revision }
  }, dropLive)

  /**
   * Put a frame on its canvas, and the stroke's next frame on the same canvas.
   * Size, place and pixels are written in one step, the place on the canvas's
   * own style rather than through reactive bindings, so a frame never shows
   * its new pixels at its old place.
   *
   * @param {HTMLCanvasElement} node
   * @param {Frame} frame
   */
  function draw(node, frame) {
    /** @type {CanvasImageSource|null} */
    let shown = null
    /** @param {Frame} next */
    const put = (next) => {
      // A frame that has just been held is the picture already here, and
      // setting a canvas's size clears it: it is left alone.
      if (next.image === shown) return
      shown = next.image
      const image = /** @type {ImageBitmap|HTMLImageElement} */ (next.image)
      node.width = image.width
      node.height = image.height
      node.style.left = `${next.x}%`
      node.style.top = `${next.y}%`
      node.style.width = `${next.w}%`
      node.style.height = `${next.h}%`
      node.getContext('2d')?.drawImage(image, 0, 0)
    }
    put(frame)
    return { update: put }
  }

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
      if (landed) free(frame.image)
      return !landed
    })
    if (kept.length !== held.length) held = kept
  }
  $effect(() => { version; busy; held; root; untrack(release) })
</script>

<div class="paints" bind:this={root} aria-hidden="true">
  {#each frames as frame (frame.id)}
    <canvas class="paint" width="0" height="0" use:draw={frame}></canvas>
  {/each}
</div>

<style>
  .paints {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }

  .paint {
    position: absolute;
  }
</style>
