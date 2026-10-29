<script>
  import { tick } from 'svelte'
  import {
    editor,
    pages,
    scopePageIndices,
    scopedRegions,
    toggleReviewFilter,
    select,
  } from '../state/editor.svelte.js'
  import { flaggedCount, maskRows } from './maskrows.js'
  import { padPosition } from './pagerows.js'
  import { deleteRow } from './maskactions.svelte.js'
  import { Empty } from '../ui/index.js'
  import { focusAndReveal, reveal } from '../ui/reveal.js'
  import { t } from '../i18n/index.js'
  import MaskRow from './MaskRow.svelte'
  import ReviewFilter from './ReviewFilter.svelte'

  /**
   * The Layers window's contents, where the same thing is called the Masks
   * panel - the design file and the user's screenshot both label the window
   * LAYERS, and the code keeps calling the contents masks.
   *
   * The `Needs review` filter heads it, then the rows: one per region, newest
   * first, or - filtered - exactly the review set in the order the
   * bottom-right pill's arrows step through it. Both orderings come from
   * `model/review.js` via `maskrows.js`; the panel does not sort.
   *
   * Scope is the open page, or in longstrip the strip's current viewport,
   * which the panel says out loud rather than leaving the user to wonder why
   * a region two screens down is missing.
   *
   * **Keyboard.** Arrows and Home/End move between rows, Enter or Space
   * expands one (the summary is a real button), Delete or Backspace removes
   * what the row stands for - its mask, or the region itself when it has none
   * - and every control on a row is an ordinary tab stop.
   */

  /** @type {HTMLElement|undefined} */
  let listEl = $state()
  /** @type {HTMLElement|undefined} */
  let headEl = $state()
  /** @type {string|null} */
  let expandedId = $state(null)

  const longstrip = $derived(editor.project?.mode === 'longstrip')
  const regions = $derived(scopedRegions())
  const byId = $derived(new Map(regions.map((region) => [region.id, region])))
  const flagged = $derived(flaggedCount(regions))
  const rows = $derived(maskRows(regions, { filtered: editor.reviewFilter }))

  const scopeNote = $derived.by(() => {
    const positions = scopePageIndices().map((index) => padPosition(pages()[index]?.number ?? index + 1))
    return positions.length > 1
      ? t('masks.scope.viewport', { from: positions[0], to: positions.at(-1) })
      : t('masks.scope.viewportOne', { position: positions[0] ?? '' })
  })

  const emptyKey = $derived(
    editor.reviewFilter
      ? 'masks.empty.noneNeedReview'
      : editor.chapter?.noTextDetected
        ? 'masks.empty.noText'
        : 'masks.empty.noMasks',
  )

  // An expanded row is about one region on one page; changing either is
  // leaving that row behind.
  $effect(() => {
    editor.pageIndex
    editor.reviewFilter
    expandedId = null
  })

  // The panel follows the selection, wherever it was made: a region clicked on
  // the canvas, an arrow on the bottom-right pill, a row expanded here. The
  // list is a scrolling column and the region being worked on has to be in it,
  // or the panel is answering a question about something off screen.
  //
  // Keyed to `selectionId` rather than `reviewCurrentId` - the pill's arrows
  // write both (`stepReview`), and the canvas writes only the first. `nearest`
  // is what makes it safe to key it this broadly: a row already in view is not
  // moved, so expanding one does not scroll the list out from under the
  // pointer. Scrolling only, never focus: the click that selected the region
  // was on the canvas, and pulling the focus into the panel would take the
  // keyboard away from the page the user is looking at.
  //
  // `reveal` and not `scrollIntoView`: this effect runs on every press on a
  // row - `MaskRow` selects on `onpointerdown` for the whole row, so Retry,
  // Delete and the engine picker all reach here - and `scrollIntoView` scrolls
  // every scrollable ancestor, `.editor` among them when the window hangs
  // below it. That is the 137.5px canvas jump.
  // `reveal` moves this list and nothing else, with the same `nearest` rule.
  $effect(() => {
    const id = editor.selectionId
    if (!id || !listEl) return
    const row = listEl.querySelector(`[data-mask-row="${CSS.escape(id)}"]`)
    if (row instanceof HTMLElement) reveal(row)
  })

  /**
   * @param {string} id
   * @param {boolean} open
   */
  function toggle(id, open) {
    expandedId = open ? id : null
    select(id)
  }

  /**
   * Keyboard moves use `focusAndReveal` (`../ui/reveal.js`), for both halves
   * of the same hazard. `HTMLElement.focus()` scrolls the element into view by
   * default and scrolls **every** scrollable ancestor to do it: deleting the
   * last row hands focus to the filter button at the top of the panel, and
   * that scroll threw the list back to the top - a row deleted near the bottom
   * took the reader's place in the list with it. And the `scrollIntoView` that
   * used to bring the next row into view walked the same ancestors, which is
   * how a press in this window moved the canvas. `preventScroll` plus a reveal
   * that touches only this list keeps the focus, keeps the `nearest` semantics,
   * and moves nothing else.
   */

  /** The rows' summary buttons, in order - the list's focus ring. */
  function summaries() {
    const wrappers = listEl?.querySelectorAll('[data-mask-row]') ?? []
    return Array.from(wrappers)
      .map((wrapper) => wrapper.querySelector('button'))
      .filter((button) => button instanceof HTMLElement)
  }

  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    if (event.metaKey || event.ctrlKey || event.altKey) return
    const buttons = summaries()
    const at = buttons.indexOf(/** @type {any} */ (event.target))
    // Focus is inside an expanded row's own controls; those keys are theirs.
    if (at === -1) return

    if (event.key === 'Delete' || event.key === 'Backspace') {
      const region = byId.get(rows[at]?.id)
      if (!region) return
      event.preventDefault()
      // The row goes; the one that takes its place keeps the focus - and only
      // once it exists, or focus falls out of the panel entirely. Delete the
      // last row and there is no such row: focus goes to the filter at the top
      // of the panel, which is the nearest thing that is still there.
      deleteRow(region).then(async (deleted) => {
        if (!deleted) return
        await tick()
        const remaining = summaries()
        const row = remaining.length > 0 ? remaining[Math.min(at, remaining.length - 1)] : null
        const next = row ?? headEl?.querySelector('button')
        // The head button is already at the top of the panel; only a row is
        // worth revealing.
        if (next instanceof HTMLElement) focusAndReveal(next, !!row)
      })
      return
    }

    /** @type {Record<string, number>} */
    const targets = {
      ArrowDown: at + 1,
      ArrowUp: at - 1,
      Home: 0,
      End: buttons.length - 1,
    }
    const next = targets[event.key]
    if (next === undefined) return
    event.preventDefault()
    const target = buttons[Math.min(buttons.length - 1, Math.max(0, next))]
    if (target instanceof HTMLElement) focusAndReveal(target, true)
  }
</script>

<div class="head" bind:this={headEl}>
  <ReviewFilter count={flagged} active={editor.reviewFilter} ontoggle={toggleReviewFilter} />
  {#if longstrip}
    <p class="scope">{scopeNote}</p>
  {/if}
</div>

{#if rows.length === 0}
  <Empty size="sm" title={t(emptyKey)} />
{:else}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div bind:this={listEl} class="list" role="list" {onkeydown}>
    {#each rows as row (row.id)}
      <MaskRow
        {row}
        region={byId.get(row.id)}
        open={expandedId === row.id}
        ontoggle={(open) => toggle(row.id, open)}
      />
    {/each}
  </div>
{/if}

<style>
  /* The window gives the panel one scrolling body, so the filter stays put at
     the top of it rather than scrolling away from the list it filters. */
  .head {
    position: sticky;
    top: 0;
    z-index: 1;
    padding: var(--s-3) 2px 7px;
    background: var(--panel);
  }

  .scope {
    margin: 7px 0 0;
    font-size: 10.5px;
    color: var(--t3);
    line-height: 1.4;
  }

  .list { display: flex; flex-direction: column }
</style>
