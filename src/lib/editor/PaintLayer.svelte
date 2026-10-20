<script>
  import { untrack } from 'svelte'
  import { draft } from './draft.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { pageVersion } from '../api/tile.js'
  import { brushFromParams, nativePoints } from './paintplan.js'
  import { brushSurface } from './brushsurface.js'
  import { cleanedTiles, registerPaintPreview } from './paintpreview.js'
  import { keepEraseMasks, showEraseMask } from './erasepreview.svelte.js'

  /**
   * The pixels a Brush or clone stroke is laying down, before the page's own
   * tiles show them.
   *
   * **Only the paint, on a canvas.** A frame is a see-through picture of the
   * painted pixels and nothing else, on the tiles' own proxy grid. So nothing
   * in this layer has a background, and the art around the stroke is never
   * drawn a second time.
   *
   * **Drawn here, as the pointer moves.** Each stroke has a surface
   * (`brushsurface.js`) fed the draft's new points; it plans the dabs the
   * commit will stamp and keeps the stroke's coverage. A Brush stroke shows
   * that coverage in its one colour. A clone stroke shows the cleaned tiles
   * already on the sheet, moved by the clone's offset, through it: the page
   * as it stood before the stroke, which is what `render_clone` reads. Asking
   * the native side for a frame per move opened the job, decoded the whole
   * page and rendered the stroke from its first point, and a fast drag
   * trailed far behind the pointer.
   *
   * **Two kinds of frame.** `live` is the stroke being drawn. When that stroke
   * is committed it is **held**: kept on screen until the tiles under it have
   * loaded a version that includes it, so the paint never blinks out between
   * the gesture ending and the page catching up. A stroke has one canvas from
   * its first frame until it is let go, live or held, and a held frame is
   * never drawn again: the tiles under a clone may already be the new ones.
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
   * become tiles. `strip` lets a Brush stroke run past the page's top and
   * bottom onto its neighbours, as a longstrip commit does. A clone commit
   * stays on its page (`paint/mod.rs#window_of`), and so does its preview.
   *
   * **The eraser has frames too, never shown.** Its canvas holds the stroke's
   * coverage, and the page's Brush layers take it off themselves
   * (`erasepreview.svelte.js`). A committed eraser stroke is held only until
   * its commit returns: by then each layer it changed has a new picture
   * coming, and a layer keeps its erased look until that picture is drawn.
   *
   * @type {{
   *   page: import('../api/backend.js').ApiPage,
   *   busy?: number,
   *   dropped?: number,
   *   strip?: boolean,
   * }}
   */
  let { page, busy = 0, dropped = 0, strip = false } = $props()

  /**
   * `id` is the stroke's, and so its canvas's. The box is in page percent.
   * `paint` brings a canvas up to date, and `tick` moves each time it has
   * more to show.
   *
   * `erase` is an eraser stroke's mask id: its canvas is coverage, not paint.
   *
   * @typedef {{id: number, paint: (node: HTMLCanvasElement) => void, tick: number, x: number, y: number, w: number, h: number, revision: string, erase?: number}} Frame
   */

  let root = $state()
  /** @type {Frame|null} */
  let live = $state.raw(null)
  /** @type {Array<Frame & {initial: Map<string|undefined, string|undefined>}>} */
  let held = $state.raw([])
  /** One list, the live frame on top, so a frame keeps its canvas when it is held. */
  const frames = $derived(live ? [...held, live] : held)
  let strokes = 0
  let erasures = 0
  let previousDraft = null
  let seenDropped = 0

  const active = $derived(draft.active?.pageId === page.id ? draft.active : null)
  const version = $derived(pageVersion(page, 'cleaned'))
  const pageId = $derived(page.id)
  const drawing = $derived(active?.kind === 'stroke' && (active.tool === 'cloneStamp' || active.tool === 'eraser' ||
    (active.tool === 'brush' && editor.toolParams.brush?.mode === 'paint')))

  /**
   * The stroke being drawn: its surface, which draft and parameters it was
   * started for, how many of the draft's points it has taken, and how it is
   * put on a canvas.
   *
   * @type {{surface: ReturnType<typeof brushSurface>, draft: object, key: string, taken: number, paint: Frame['paint'], mask: number|undefined}|null}
   */
  let stroking = null
  let strokeFrame = 0

  /** Forget the stroke being drawn. */
  function dropLive() {
    cancelAnimationFrame(strokeFrame)
    strokeFrame = 0
    stroking = null
    live = null
  }

  /** The stroke was committed: keep its last frame until the tiles show it. */
  function hold() {
    // Only a frame still waiting for the display is drawn: the one on
    // screen is already the stroke's last.
    if (strokeFrame) show()
    stroking = null
    if (!live) return
    const initial = new Map()
    for (const tile of tilesUnder(live)) initial.set(tile.dataset.paintKey, tile.dataset.version)
    held = [...held, { ...live, initial }]
    live = null
  }

  function dropHeld() {
    held = []
  }

  /**
   * Bring a canvas up to date with its frame. A frame it already shows - one
   * that has just been held - is left alone.
   *
   * @param {HTMLCanvasElement} node
   * @param {Frame} frame
   */
  function draw(node, frame) {
    let shown = -1
    /** @param {Frame} next */
    const put = (next) => {
      if (next.tick === shown) return
      shown = next.tick
      next.paint(node)
    }
    put(frame)
    return { update: put }
  }

  /**
   * A whole page pixel, rounded as `render_clone` rounds its offset (half
   * away from zero).
   *
   * @param {number} value
   */
  const whole = (value) => Math.sign(value) * Math.round(Math.abs(value))

  /**
   * Feed the draft's new points to its surface, starting a surface for a new
   * stroke or for changed parameters (a `[` mid-stroke resizes the whole
   * stroke, as the commit will), and ask for a frame.
   */
  function extend() {
    if (!active) return
    const clone = active.tool === 'cloneStamp'
    const erase = active.tool === 'eraser'
    // Where a clone reads from was fixed when it started, and the commit
    // sends the same offset (`drawing.svelte.js#cloneInto`).
    if (clone && !active.clone) return
    const params = editor.toolParams[active.tool] ?? {}
    const key = JSON.stringify([params, active.clone, version])
    const width = Number(page.width) || 1600
    const height = Number(page.height) || 2400
    if (stroking?.draft !== active || stroking.key !== key) {
      const surface = brushSurface({ brush: brushFromParams(params), color: String(params.color ?? '#000000'),
        pageWidth: width, pageHeight: height, clipY: clone || !strip, buildUp: clone })
      let paint = (/** @type {HTMLCanvasElement} */ node) => surface.paint(node)
      let mask = undefined
      if (erase) {
        const id = mask = (erasures += 1)
        let tick = 0
        paint = (node) => {
          surface.paint(node)
          const box = surface.bounds()
          if (box) showEraseMask(page.id, { id, canvas: node, box, tick: (tick += 1) })
        }
      }
      if (clone) {
        const mask = document.createElement('canvas')
        const shift = { x: whole((active.clone.offset.x / 100) * width), y: whole((active.clone.offset.y / 100) * height) }
        paint = (node) => surface.paintClone(node, { mask, tiles: cleanedTiles(root?.closest('.sheet')), shift })
      }
      stroking = { surface, draft: active, key, taken: 0, paint, mask }
    }
    const fresh = active.points.slice(stroking.taken)
    if (fresh.length === 0) return
    stroking.taken = active.points.length
    for (const point of nativePoints(fresh, width, height)) stroking.surface.add(point)
    if (!strokeFrame) strokeFrame = requestAnimationFrame(show)
  }

  /** Put the stroke's latest pixels on screen, one frame per display frame. */
  function show() {
    cancelAnimationFrame(strokeFrame)
    strokeFrame = 0
    const bounds = stroking?.surface.bounds()
    if (!stroking || !bounds) return
    live = { id: live?.id ?? (strokes += 1), paint: stroking.paint, tick: (live?.tick ?? 0) + 1, ...bounds, revision: version, erase: stroking.mask }
  }

  $effect(() => {
    const id = pageId
    const onLoad = () => {
      release()
      // A tile that arrives mid-stroke has pixels a clone shows.
      if (stroking && !strokeFrame) strokeFrame = requestAnimationFrame(show)
    }
    document.addEventListener('paint-tile-loaded', onLoad)
    return () => {
      document.removeEventListener('paint-tile-loaded', onLoad)
      previousDraft = null
      dropLive()
      dropHeld()
      keepEraseMasks(id, new Set())
    }
  })

  // Take the final pointer position before the commit drops the draft.
  $effect(() => registerPaintPreview(page.id, async () => {
    if (drawing) { extend(); show() }
  }))
  $effect(() => {
    // Read all dependencies here; lifecycle changes come first so starting a
    // stroke cannot drop its own first frame. A stroke reads only its length:
    // the points before are already on its surface.
    const current = drawing ? active : null
    const key = current ? JSON.stringify([current.points.length, current.clone, editor.toolParams[current.tool], version]) : ''
    untrack(() => {
      if (current !== previousDraft) {
        if (previousDraft?.committed) hold()
        else dropLive()
        previousDraft = current
      }
      if (key) extend()
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
      if (frame.erase !== undefined) return false
      if (version === frame.revision) return true
      const landed = tilesUnder(frame).every(tile =>
        tile.dataset.loadedVersion === tile.dataset.version &&
        frame.initial.get(tile.dataset.paintKey) !== tile.dataset.version)
      return !landed
    })
    if (kept.length !== held.length) held = kept
  }
  $effect(() => { version; busy; held; root; untrack(release) })

  // An eraser stroke is shown for as long as its frame lives.
  $effect(() => {
    const ids = new Set(frames.map(frame => frame.erase).filter(id => id !== undefined))
    untrack(() => keepEraseMasks(pageId, ids))
  })
</script>

<div class="paints" bind:this={root} aria-hidden="true">
  {#each frames as frame (frame.id)}
    <canvas class="paint" class:erase={frame.erase !== undefined} width="0" height="0" use:draw={frame}></canvas>
  {/each}
</div>

<style>
  .paints {
    position: absolute;
    z-index: var(--z-paint);
    inset: 0;
    pointer-events: none;
  }

  .paint {
    position: absolute;
  }

  .erase {
    visibility: hidden;
  }
</style>
