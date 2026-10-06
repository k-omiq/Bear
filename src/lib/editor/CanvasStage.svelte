<script>
  import { flushSync, untrack } from 'svelte'
  import {
    editor,
    pages,
    currentPage,
    consumeStripScroll,
    reportFitScale,
    reportScrollRoom,
    setStripPosition,
    setStripScope,
    setZoom,
    MIN_ZOOM,
    MAX_ZOOM,
  } from '../state/editor.svelte.js'
  import {
    anchoredScroll,
    fitScale,
    fitWidth,
    naturalWidth,
    pageRatio,
    sheetWidth,
    wheelZoom,
  } from './zoom.js'
  import {
    PRELOAD_SCREENS,
    STRIP_GAP,
    centreIndex,
    scrollTopFor,
    stripMetrics,
    stripWindow,
    visibleIndices,
  } from './strip.js'
  import { Empty } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import { draft } from './draft.svelte.js'
  import PageSheet from './PageSheet.svelte'

  /**
   * The canvas: the page column inside the editor's viewport.
   * The viewport itself - the screen's only scroll container, its `--bg`
   * field and its `70px 104px 66px` - belongs to `EditorScreen.svelte` and
   * stays there. **A second scroll container here would break autosave's
   * restore**, so this component reaches the shell's scroller
   * (`stageEl.parentElement`, the idiom `PageList` already uses) rather than
   * growing one.
   *
   * What it owns is everything the chrome cannot know:
   *
   * - **Fit.** The sheet's width comes from the viewport's content box, not
   *   from a constant. The unit is the fraction of the page's own pixels the
   *   sheet is drawn at, so the bottom pill's readout is the truth; with `fit`
   *   on, the computed scale goes to `editor.fitScale` through
   *   `reportFitScale` and `displayZoom()` answers for both cases.
   * - **Pinch and modifier-scroll**, anchored under the pointer. A plain wheel
   *   is left alone and keeps scrolling.
   * - **The longstrip column**: a virtual window over the pages, the position
   *   readout following the viewport's centre, and the scroll request
   *   `goToPage` leaves for it.
   * - **Preloading.** A longstrip mounts the pages and tiles a screen past
   *   either end of the viewport; a paginated chapter fetches the pages either
   *   side of the one on screen.
   * - **Scroll room.** Half a viewport of empty canvas above and below the
   *   page, so its top and bottom can be pulled to the middle of the screen,
   *   clear of the top bar and of a floating window. A margin on the stage,
   *   not padding on the viewport, whose padding is what "fit" measures. A
   *   fitted single page has none: it is all on screen and should not move.
   *   The room is reported (`reportScrollRoom`) so that a saved position is
   *   measured from the page's place at rest, and when the room changes the
   *   scroller moves by the same amount, so the page stays where it is.
   */

  /** @type {HTMLElement|undefined} */
  let stageEl = $state()

  /* The viewport's content box, for fit. */
  let contentWidth = $state(0)
  let contentHeight = $state(0)

  /* The column's position, for the strip. Measured against the scroller's
     padding box, so the viewport's own padding never enters the arithmetic
     twice. `columnTop` goes negative as the reader scrolls into the column. */
  let columnTop = $state(0)
  let viewportHeight = $state(0)
  let columnOrigin = 0
  let focusedIndex = $state(-1)

  const list = $derived(pages())
  const longstrip = $derived(editor.project?.mode === 'longstrip')
  // The single page's model. A longstrip column has no single model page: it
  // is measured page by page through `stripMetrics`, because a chapter's
  // segments are not uniform by construction.
  const model = $derived(longstrip ? null : currentPage())

  // What "fit" is fitting. For one page that is the page; for a column it is
  // the **widest** page in it, so that fitting the column fits every page in
  // it rather than only the first. `ratio` is the single page's, and enters
  // `fitWidth` only through its height term, which longstrip does not use.
  const natural = $derived(longstrip ? widest(list) : naturalWidth(model))
  const ratio = $derived(pageRatio(longstrip ? null : model))
  const fitSpec = $derived({ contentWidth, contentHeight, ratio, longstrip, natural })
  const fitZoom = $derived(fitScale(fitSpec))
  const zoom = $derived(editor.fit ? fitZoom : editor.zoom)

  // Fit draws from the *exact* fit width, not from `fitZoom`: the reported
  // scale is rounded to two places so the readout is stable, and at a natural
  // width of 1600 those two places are 16px of sheet - enough to overflow the
  // viewport a fit is supposed to fit inside. Expressed as a scale rather than
  // as a width, because every page in a column is drawn at the same *scale*
  // and at its own width.
  const scale = $derived(editor.fit ? fitWidth(fitSpec) / natural : editor.zoom)
  const width = $derived(sheetWidth({ natural, zoom: scale }))

  const metrics = $derived(
    stripMetrics({ pages: longstrip ? list : [], scale, gap: STRIP_GAP }),
  )

  // How far past the viewport pages and their tiles are fetched ahead.
  const margin = $derived(viewportHeight * PRELOAD_SCREENS)
  const band = $derived(
    stripWindow({ metrics, columnTop, viewportHeight, margin, include: focusedIndex }),
  )
  // The band is recomputed on every scroll event; its bounds rarely change.
  // Read through primitives, so the mounted list is rebuilt only when they do.
  const bandStart = $derived(band.start)
  const bandEnd = $derived(band.end)
  const bandExtra = $derived(band.extra)
  const visible = $derived.by(() => {
    if (!longstrip) return []
    const indices = Array.from({ length: bandEnd - bandStart }, (_, i) => bandStart + i)
    if (bandExtra !== null) indices.push(bandExtra)
    return indices.sort((a, b) => a - b).map((index) => ({ page: list[index], index }))
  })

  /**
   * Where a viewport-relative line falls on strip page `index`, in percent of
   * the page's height: how the preload margin is handed to the page's tiles.
   *
   * @param {number} index
   * @param {number} y px below the viewport's top edge
   * @returns {number}
   */
  function loadEdge(index, y) {
    const height = metrics.units[index] - STRIP_GAP
    return height > 0 ? ((y - columnTop - metrics.offsets[index]) / height) * 100 : 0
  }

  // Mount the current page and one neighbour either side, keyed by page ID.
  // Both source and native cleaned tiles decode ahead of navigation, and a
  // page turn reveals the same loaded image nodes instead of recreating them.
  const paginated = $derived(longstrip ? [] : list.slice(
    Math.max(0, editor.pageIndex - 1), editor.pageIndex + 2,
  ))

  $effect(() => {
    editor.chapter?.id
    focusedIndex = -1
  })

  /**
   * The widest page in the column, at 1:1. Not `list[0]`: a chapter whose
   * first segment is narrower than a later one would fit the viewport to the
   * narrow one and push the wide one under the floating windows.
   *
   * @param {ReadonlyArray<{width?: number}>} pages
   * @returns {number}
   */
  function widest(pages) {
    let out = 0
    for (const page of pages) out = Math.max(out, naturalWidth(page))
    return out || naturalWidth(null)
  }

  /* ---------------------------------------------------------------- */
  /* Measurement                                                       */
  /* ---------------------------------------------------------------- */

  /** @returns {HTMLElement|null} the shell's scroller */
  function scroller() {
    return stageEl?.parentElement ?? null
  }

  // Scrolling changes only this offset. Geometry is cached on resize, so
  // ordinary wheel and momentum scroll never force a bounding-box layout.
  function readScroll() {
    const box = scroller()
    if (box) columnTop = columnOrigin - box.scrollTop
  }

  /**
   * The viewport's content box - its padding taken off, because "fit" has to
   * fit what is actually free, and `70px 104px 66px` of it is not.
   */
  function measureViewport() {
    const box = scroller()
    if (!box || !stageEl) return
    const style = globalThis.getComputedStyle?.(box)
    const px = (/** @type {string|undefined} */ value) => Number.parseFloat(value ?? '') || 0
    const width = box.clientWidth - px(style?.paddingLeft) - px(style?.paddingRight)
    const height = box.clientHeight
    const origin = stageEl.getBoundingClientRect().top - box.getBoundingClientRect().top + box.scrollTop
    contentWidth = width
    contentHeight = height - px(style?.paddingTop) - px(style?.paddingBottom)
    viewportHeight = height
    columnOrigin = origin
    readScroll()
  }

  // Measured once outright and then on every resize. The first measurement is
  // not left to the observer: a `ResizeObserver` notifies during the browser's
  // rendering steps, which a background tab does not run, and a canvas that
  // draws at its minimum width until the tab is looked at is not acceptable.
  // The column is observed too: a zoom or a chapter load resizes it, and the
  // viewport centres a short column, which moves its origin without a
  // viewport resize.
  $effect(() => {
    const box = scroller()
    if (!box || !stageEl) return
    measureViewport()
    if (!globalThis.ResizeObserver) return
    const observer = new ResizeObserver(measureViewport)
    observer.observe(box)
    observer.observe(stageEl)
    return () => observer.disconnect()
  })

  $effect(() => {
    reportFitScale(fitZoom)
  })

  const room = $derived(editor.fit && !longstrip ? 0 : Math.round(viewportHeight / 2))
  let roomApplied = 0

  // The margin and the scroll that makes up for it are written together,
  // here and not in the markup: an effect can run before the markup has caught
  // up with the state it read, and a scroll corrected against the old margin
  // moves the page. A margin does not resize the stage, so no observer fires
  // for it: the column's origin is measured again here.
  $effect(() => {
    const next = room
    untrack(() => {
      const box = scroller()
      if (!box || !stageEl || next === roomApplied) return
      stageEl.style.marginTop = stageEl.style.marginBottom = next ? `${next}px` : ''
      box.scrollTop += next - roomApplied
      roomApplied = next
      reportScrollRoom(next)
      measureViewport()
    })
  })

  $effect(() => () => reportScrollRoom(0))

  // Where the column sits relative to the viewport, for the strip only. The
  // shell's own `onscroll` runs first (it is registered first) and records the
  // settle through `setScroll`; this only reads.
  $effect(() => {
    if (!longstrip) {
      untrack(() => setStripScope([]))
      return
    }
    const box = scroller()
    if (!box) return
    readScroll()
    box.addEventListener('scroll', readScroll, { passive: true })
    return () => box.removeEventListener('scroll', readScroll)
  })

  // The strip reports what it has on screen, what it has mounted and which
  // page is at the centre. None goes through `goToPage`, which resets scroll
  // to {0,0} and would fight the reader for the scrollbar. The mounted pages
  // are reported so their regions - and with them their patch layers - are in
  // hand before they scroll into view.
  $effect(() => {
    if (!longstrip || editor.loading || list.length === 0) return
    const spec = { metrics, columnTop, viewportHeight }
    const focus = focusedIndex
    const mounted = visible.map((entry) => entry.index)
    // Scope, mounted band and centre describe one viewport. Loading between
    // their writes fetched the same pages twice and tracked the resident cache
    // in this effect.
    untrack(() => setStripPosition(centreIndex(spec), visibleIndices(spec), focus, mounted))
  })

  // The other half of the handshake: the Pages list, the bottom pill and
  // `stepReview` all ask for a position through `goToPage`, which cannot know
  // where that position sits in the column and leaves a request instead.
  $effect(() => {
    const request = editor.stripScrollRequest
    if (!request || !longstrip) return
    untrack(() => {
      const box = scroller()
      if (!box) return
      consumeStripScroll()
      box.scrollTop = scrollTopFor({
        index: request.index,
        metrics,
        scrollTop: box.scrollTop,
        columnTop,
      })
      readScroll()
    })
  })

  /* ---------------------------------------------------------------- */
  /* Zoom                                                              */
  /* ---------------------------------------------------------------- */

  /**
   * A `wheel` with `ctrlKey` is both a trackpad pinch and a Ctrl/⌘-scroll, and
   * both mean zoom. Anything else is a scroll and is left alone - which is why
   * `preventDefault` is on the zooming path only, and why the listener is
   * registered `{ passive: false }` (a passive listener may not prevent).
   *
   * **One zoom per frame.** A trackpad sends wheel events faster than the
   * screen draws, and each zoom re-lays the whole column and measures it.
   * Events only accumulate into `pinchZoom`; the next animation frame applies
   * the result once, anchored under the latest pointer.
   *
   * `pinchZoom` is the gesture's own *unrounded* scale. `setZoom` keeps two
   * places, so starting every event from the kept value rounded a slow pinch's
   * every step back to where it began, and the page did not move at all.
   *
   * @param {WheelEvent} event
   */
  function onwheel(event) {
    if (!event.ctrlKey && !event.metaKey) return
    event.preventDefault()
    if (!scroller() || !stageEl) return

    // Continue the gesture only while the page is still at the scale it last
    // set: a button, a key or fit in between starts from what is on screen.
    const from = pinchFrame || zoom === pinchKept ? pinchZoom : zoom
    const target = wheelZoom({ deltaY: event.deltaY, zoom: from, min: MIN_ZOOM, max: MAX_ZOOM })
    if (target === null) return
    pinchZoom = target
    pinchAt = { x: event.clientX, y: event.clientY }
    if (!pinchFrame) pinchFrame = requestAnimationFrame(applyPinch)
  }

  let pinchZoom = 1
  let pinchKept = Number.NaN
  let pinchAt = { x: 0, y: 0 }
  let pinchFrame = 0

  function applyPinch() {
    pinchFrame = 0
    const box = scroller()
    if (!box || !stageEl) return
    const before = boxOf(stageEl)
    setZoom(pinchZoom)
    pinchKept = editor.zoom
    // The scale change and the scroll correction have to land in the same
    // frame, or the page jumps and then slides.
    flushSync()
    const next = anchoredScroll({
      pointerX: pinchAt.x,
      pointerY: pinchAt.y,
      before,
      after: boxOf(stageEl),
      scrollLeft: box.scrollLeft,
      scrollTop: box.scrollTop,
    })
    box.scrollLeft = next.left
    box.scrollTop = next.top
    readScroll()
  }

  $effect(() => {
    const box = scroller()
    if (!box) return
    box.addEventListener('wheel', onwheel, { passive: false })
    return () => {
      box.removeEventListener('wheel', onwheel)
      if (pinchFrame) cancelAnimationFrame(pinchFrame)
      pinchFrame = 0
    }
  })

  /**
   * @param {HTMLElement} element
   * @returns {import('./zoom.js').Box}
   */
  function boxOf(element) {
    const rect = element.getBoundingClientRect()
    return { left: rect.left, top: rect.top, width: rect.width, height: rect.height }
  }
</script>

<div
  class="stage"
  bind:this={stageEl}
  style:width={longstrip && list.length ? `${width}px` : undefined}
  style:height={longstrip && list.length ? `${metrics.total}px` : undefined}
  onfocusin={(event) => {
    const slot = event.target instanceof Element ? event.target.closest('[data-strip-index]') : null
    focusedIndex = slot ? Number(slot.getAttribute('data-strip-index')) : -1
  }}
  onfocusout={(event) => {
    if (!stageEl?.contains(/** @type {Node|null} */ (event.relatedTarget))) focusedIndex = -1
  }}
>
  {#if editor.loading}
    <Empty title={t('editor.state.opening')} />
  {:else if list.length === 0}
    <Empty title={t('editor.state.noPages')} />
  {:else if longstrip}
    {#each visible as { page, index } (page.id)}
      <div
        class="slot"
        data-strip-index={index}
        style:top="{metrics.offsets[index]}px"
        style:left="{(width - metrics.widths[index]) / 2}px"
        class:selection-origin={page.regions?.some((region) => region.id === editor.selectionId)}
        class:hover-origin={page.regions?.some((region) => region.id === editor.hoverId)}
        class:gesture-origin={draft.active?.pageId === page.id}
      >
        <PageSheet
          {page}
          width={metrics.widths[index]}
          strip
          current={index === editor.pageIndex}
          tabbable={index === editor.pageIndex}
          stripMinY={index > 0 ? -(Number(list[index - 1]?.height) || 1) / (Number(page.height) || 1) * 100 : 0}
          stripMaxY={index + 1 < list.length ? 100 + (Number(list[index + 1]?.height) || 1) / (Number(page.height) || 1) * 100 : 100}
          loadFrom={loadEdge(index, -margin)}
          loadTo={loadEdge(index, viewportHeight + margin)}
        />
      </div>
    {/each}
  {:else if model}
    {#each paginated as page (page.id)}
      <div class="paginated" hidden={page.id !== model.id} data-page-id={page.id}>
        <PageSheet {page} {width} current={page.id === model.id} tabbable={page.id === model.id} />
      </div>
    {/each}
  {/if}
</div>

<style>
  .stage {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    flex: none;
  }

  /* The whole column owns its geometry, even when the widest sheet is absent.
     Fixed offsets also avoid spacer rounding and scroll anchoring corrections. */
  .slot {
    position: absolute;
  }

  /* Cross-position drafts and selected region outlines belong to their
     starting sheet. Keep them above the following sheet's artwork. */
  .slot.selection-origin { z-index: 1 }
  .slot.hover-origin { z-index: 2 }
  .slot.gesture-origin { z-index: 3 }

</style>
