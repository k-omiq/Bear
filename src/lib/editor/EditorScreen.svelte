<script>
  import { tick, untrack } from 'svelte'
  import { app } from '../state/app.svelte.js'
  import { session, clampWindowsToViewport } from '../state/session.svelte.js'
  import {
    editor,
    openEditorChapter,
    closeEditorChapter,
    consumeResume,
    setScroll,
  } from '../state/editor.svelte.js'
  import { t } from '../i18n/index.js'
  import CanvasStage from './CanvasStage.svelte'
  import ClusterLeft from './ClusterLeft.svelte'
  import ClusterRight from './ClusterRight.svelte'
  import ToolRail from './ToolRail.svelte'
  import ZoomPill from './ZoomPill.svelte'
  import NavPill from './NavPill.svelte'
  import PagesWindow from './PagesWindow.svelte'
  import LayersWindow from './LayersWindow.svelte'
  import ToolBar from './ToolBar.svelte'

  /**
   * The editor: a full-viewport canvas with everything else floating over it.
   *
   * The canvas viewport is the only scroll container in the screen. The two
   * clusters, the tool rail and the two bottom pills are fixed at `z-index:40`;
   * the two windows and the tool bar sit at `20 + rank`, so a raised one
   * comes forward of the others but can never cover a fixed control.
   *
   * **DOM order is the tab order**, and it is deliberately not the visual
   * order: clusters, windows and the tool bar, rail, pills, and the canvas
   * last. A keyboard user meets what is fixed before what floats, and the
   * scroller - which has no controls of its own - last.
   *
   * They render in a fixed order and stack by `z-index` alone. Sorting
   * the markup by stacking rank would move a window's DOM node the moment it
   * was clicked or focused, which drops focus and can swallow the click that
   * caused it; a stable tab order is worth more than a tab order that tracks
   * the stack.
   *
   * This screen owns the chapter's lifecycle: `openEditorChapter` on mount and
   * on every route change, `closeEditorChapter` on teardown. The backend
   * subscription and the autosave flush both hang off that pair.
   */

  /** @type {HTMLElement|undefined} */
  let viewport = $state()

  /**
   * A resume asked for at Home starts here, once the chapter is loaded and the
   * event channel is live - never before (see `library.svelte.js#resumeProject`).
   *
   * `untrack` because `openEditorChapter` reads `editor.chapter` to decide
   * whether the route is already open: without it, the load's own write to that
   * field would re-run this effect.
   */
  $effect(() => {
    const { projectId, chapterId } = app.route
    if (!projectId || !chapterId) return
    untrack(() => {
      openEditorChapter(projectId, chapterId).then((ok) => {
        if (ok) consumeResume()
      })
    })
  })

  // A layout saved on a large display must not leave a window unreachable on a
  // small one, so every window is pulled back onto the viewport on mount and on
  // every resize.
  //
  // `untrack` because the clamp reads the geometry it writes: tracked, every
  // pixel of a drag would re-run this effect, tearing the `resize` listener
  // down and putting it back a few hundred times a gesture.
  $effect(() => {
    untrack(() => clampWindowsToViewport())
    const onresize = () => clampWindowsToViewport()
    globalThis.addEventListener?.('resize', onresize)
    return () => globalThis.removeEventListener?.('resize', onresize)
  })

  // Teardown only: flush autosave and release the backend subscription.
  $effect(() => () => closeEditorChapter())

  // Autosave restores the scroll position, so the scroller adopts it whenever
  // the chapter opens, and - in a single-page chapter - whenever the page
  // changes.
  //
  // **Not on a page change in a longstrip**, where the pages are one
  // continuous column: there `goToPage` leaves a scroll request the canvas
  // consumes (see `state/editor.svelte.js`, "The strip handshake"), and
  // re-applying the last settled position here would undo the scroll the
  // request just made. The strip owns its own scroller position; this effect
  // owns the restore.
  $effect(() => {
    // Tracked: the chapter, and the page outside a longstrip. Untracked: the
    // position, or every scroll the user made would re-run this and fight
    // them for the scrollbar.
    editor.chapter
    if (editor.project?.mode !== 'longstrip') editor.pageIndex
    // Not while the chapter is still loading: the canvas shows a placeholder
    // then, too short to scroll to a saved position, and a restore made
    // against it lands wherever the placeholder ends. The restore waits for
    // the pages, and `onscroll` records nothing until they are there.
    if (editor.loading) return
    const target = untrack(() => editor.scroll)
    adoptScroll(target.top, target.left)
  })

  /**
   * Restore a scroll position, and restore it again if the first attempt did
   * not land.
   *
   * **Why twice.** A restored `scrollTop` is only reachable if the column is
   * already as tall as it will be, and the column's height depends on a
   * measurement `CanvasStage` takes in *its* effects. This effect is created
   * in `<script>`, so it runs ahead of the child's on any flush they share; on
   * one where the canvas has not measured yet, every sheet is at
   * `MIN_SHEET_WIDTH` and a six-page strip column is 6×912 rather than 6×4012.
   * `scrollTo` would clamp against that short column, and `onscroll` would
   * then write the clamped value straight back through `setScroll` -
   * destroying the saved position rather than merely missing it.
   *
   * Today the chapter arrives from the adapter *after* mount, so by the flush
   * that carries the real position the canvas has long since measured and the
   * first pass lands (verified: 12100 against a 24208px column). The second
   * pass is what stops that being a matter of luck: it costs one `tick()` and
   * nothing at all when the first pass was already right.
   *
   * **The scroll room is read on each pass**, not once: a saved `top` is
   * measured from the page's place at rest, and the canvas reports the room
   * above it (`editor.scrollRoom`) from the same measurement. A second pass
   * aimed at the first pass's room would undo the canvas's own correction.
   *
   * @param {number} top from the page's place at rest
   * @param {number} left
   */
  async function adoptScroll(top, left) {
    const box = viewport
    if (!box) return
    box.scrollTo({ top: top + editor.scrollRoom, left })
    await tick()
    const target = top + editor.scrollRoom
    if (box.scrollTop !== target || box.scrollLeft !== left) box.scrollTo({ top: target, left })
  }

  /** @param {Event & {currentTarget: HTMLElement}} event */
  function onscroll(event) {
    // A placeholder's scroll position is not the reader's place in the chapter.
    if (editor.loading) return
    setScroll(event.currentTarget.scrollTop - editor.scrollRoom, event.currentTarget.scrollLeft)
  }
</script>

<div class="editor">
  <ClusterLeft />
  <ClusterRight />

  {#if session.windows.pages.open}<PagesWindow />{/if}
  {#if session.windows.layers.open}<LayersWindow />{/if}
  {#if session.windows.tool.open}<ToolBar />{/if}

  <ToolRail />
  <ZoomPill />
  <NavPill />

  <!-- Last in the tab order, first on the screen. A scroll container with no
       focusable content of its own is unreachable by keyboard unless it is a
       tab stop (WCAG 2.1.1), which is exactly what the compiler's rule about
       non-interactive tabindex does not know about. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    bind:this={viewport}
    class="viewport"
    role="region"
    tabindex="0"
    aria-label={t('editor.region.canvas')}
    {onscroll}
  >
    <CanvasStage />
  </div>
</div>

<style>
  .editor {
    position: relative;
    height: 100%;
    /* `clip`, not `hidden`. A `hidden` box is still a scroll *container*: it
       shows no scrollbar but it can be scrolled programmatically, and nothing
       ever scrolls it back. A floating window whose lower half hangs below
       this box (`max-height: 64vh`, and `clampPosition` clamps y by width
       only) put rows there, and `scrollIntoView`/`focus()` inside those rows
       walk every scrollable ancestor: pressing a row near the bottom of the
       Layers window took `.editor.scrollTop` from 0 to 137.5 and the
       viewport's bounding top from 0 to -137.5, so the artwork and every pill
       slid up and stayed up. `clip` cannot be scrolled at all. The viewport
       below is the one box in here that scrolls. */
    overflow: clip;
    background: var(--bg);
  }

  .viewport {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: flex-start;
    /* `safe`, and not merely `center`: a centred flex item that is wider than
       the content box loses its start-side overflow - `scrollLeft` cannot
       reach the left of the page at all, so half of every zoomed page would be
       unreachable and pointer-anchored zoom would be defeated on that half.
       `safe` falls back to start alignment exactly when the sheet overflows. */
    justify-content: safe center;
    overflow: auto;
    /* The canvas owns navigation and zoom offsets; virtual sheets must not
       invite a second scroll correction from the browser. */
    overflow-anchor: none;
    /* Clear of the clusters at the top and the pills at the bottom. */
    padding: 70px 104px 66px;
    /* A press beside the sheet lands here, not on the draw surface, and a
       drag from it onto the page started a native selection over the scans,
       which WebKit paints as the page itself selected. Nothing on the canvas
       is text to copy. */
    -webkit-user-select: none;
    user-select: none;
  }
</style>
