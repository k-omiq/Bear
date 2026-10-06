<script>
  import { tick } from 'svelte'
  import { editor, pages, goToPage } from '../state/editor.svelte.js'
  import { pageRow, rowWindow, VIRTUAL_THRESHOLD } from './pagerows.js'
  import { Empty } from '../ui/index.js'
  import { focusAndReveal } from '../ui/reveal.js'
  import { t } from '../i18n/index.js'
  import PageRow from './PageRow.svelte'

  /**
   * The Pages list - the body of the Pages window, and
   * nothing else: no frame, no header, no padding, no scroller of its own.
   *
   * A **text list**, never thumbnails: a column of manga pages competes with
   * the artwork at the moment the user is concentrating on one page. And it
   * **is the progress indicator** - the marks and the tracks tick over as the
   * adapter's events land, which is why there is no progress bar anywhere in
   * the editor.
   *
   * In longstrip an entry is a position in the strip rather than a file, and
   * selecting one scrolls the strip to it: `goToPage` is that signal, and
   * Task 9's strip is what obeys it.
   *
   * **Keyboard.** A single-select listbox with a roving tabindex: one tab
   * stop, arrows and Home/End move focus, Enter or Space opens the page. Each
   * row is a real button carrying `role="option"`, so activation is native and
   * only the arrows are handled here. Selection deliberately does *not* follow
   * focus - arrowing through a chapter would otherwise re-render the canvas
   * twenty times on the way to page 21.
   *
   * **Virtualisation.** A chapter runs to hundreds of pages, so above
   * `VIRTUAL_THRESHOLD` rows only the visible band is mounted, padded top and
   * bottom by spacers of the exact missing height. The window arithmetic is in
   * `pagerows.js` and is tested there; the scroller is the window's body, which
   * this component listens to but does not own.
   */

  /** @type {HTMLElement|undefined} */
  let listEl = $state()
  let scrollTop = $state(0)
  let viewportHeight = $state(0)
  let focusIndex = $state(0)
  let hasFocus = $state(false)

  const longstrip = $derived(editor.project?.mode === 'longstrip')
  const rows = $derived(pages().map((page, index) => pageRow(page, { index, longstrip })))
  const virtual = $derived(rows.length > VIRTUAL_THRESHOLD)

  // `include` only while the list holds focus: keeping the focused row mounted
  // is what keeps the keyboard working during a run, and there is nothing to
  // keep when nothing is focused. It comes back as `band.extra`, one row
  // rendered outside the band with the top spacer split around it, so a
  // focused row far from the scroll position costs one row rather than every
  // row in between.
  const band = $derived(
    rowWindow({
      count: rows.length,
      scrollTop,
      viewportHeight,
      include: hasFocus ? focusIndex : undefined,
    }),
  )
  const visible = $derived(rows.slice(band.start, band.end))
  const extraRow = $derived(band.extra === null ? null : rows[band.extra])

  // The list's one tab stop. While the list holds focus that is the focused
  // row, which is mounted either in the band or as `band.extra`. While it does
  // not, the focused row may be nowhere on screen - a wheel scroll moves the
  // band and not the focus - and a tab stop on an unmounted row is no tab stop
  // at all: the container is `tabindex="-1"`, so the whole list would be
  // unreachable by keyboard. Clamped into the band, the nearest rendered row
  // takes it.
  const tabStop = $derived(
    hasFocus ? focusIndex : Math.min(band.end - 1, Math.max(band.start, focusIndex)),
  )

  // The open page is where the keyboard starts, and where it returns to when
  // the page changes from anywhere else - the bottom pill, a shortcut, a
  // resumed run.
  $effect(() => {
    focusIndex = editor.pageIndex
  })

  // The window body is the scroll container (see `FloatingWindow`), so the
  // list reads its position rather than growing a second one.
  $effect(() => {
    if (!virtual || !listEl) return
    const scroller = listEl.parentElement
    if (!scroller) return
    const read = () => {
      scrollTop = scroller.scrollTop
    }
    const measure = () => {
      viewportHeight = scroller.clientHeight
      read()
    }
    measure()
    scroller.addEventListener('scroll', read, { passive: true })
    const observer = globalThis.ResizeObserver ? new ResizeObserver(measure) : null
    observer?.observe(scroller)
    return () => {
      scroller.removeEventListener('scroll', read)
      observer?.disconnect()
    }
  })

  /** @param {number} index */
  async function focusRow(index) {
    const next = Math.min(rows.length - 1, Math.max(0, index))
    if (next === focusIndex) return
    focusIndex = next
    // The row may not be mounted yet: `band` includes `focusIndex`, but only
    // once the derived has re-run.
    await tick()
    const element = listEl?.querySelector(`[data-index="${next}"]`)
    // Both halves through `focusAndReveal`: the plain `focus()` and the
    // `scrollIntoView` that used to be here each scroll *every* scrollable
    // ancestor, and the editor shell counted as one - a keyboard move in a
    // window hanging below the canvas area moved the canvas with it. This
    // scrolls the page list alone.
    if (element instanceof HTMLElement) focusAndReveal(element)
  }

  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    if (event.metaKey || event.ctrlKey || event.altKey) return
    switch (event.key) {
      case 'ArrowDown':
        focusRow(focusIndex + 1)
        break
      case 'ArrowUp':
        focusRow(focusIndex - 1)
        break
      case 'Home':
        focusRow(0)
        break
      case 'End':
        focusRow(rows.length - 1)
        break
      // A listbox owns its selection keys (WAI-ARIA APG). Handling them here
      // and preventing the default keeps one path in: without the prevent, the
      // row's own button would also fire and open the page twice.
      case 'Enter':
      case ' ':
        goToPage(focusIndex)
        break
      default:
        return
    }
    event.preventDefault()
  }
</script>

{#if rows.length === 0}
  <Empty size="sm" title={t('editor.state.noPages')} />
{:else}
  <!-- The options carry the roving tabindex; the box itself is never a tab
       stop, only a focus target for a click on the gap between rows. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
  <div
    bind:this={listEl}
    class="list"
    role="listbox"
    aria-label={t('editor.panel.pages')}
    aria-orientation="vertical"
    tabindex="-1"
    {onkeydown}
    onfocusin={() => (hasFocus = true)}
    onfocusout={(event) => {
      if (!listEl?.contains(/** @type {Node|null} */ (event.relatedTarget))) hasFocus = false
    }}
  >
    {#if band.padTop > 0}{@render spacer(band.padTop)}{/if}
    {#if extraRow && band.extraBefore}
      {@render option(extraRow)}
      {#if band.padGap > 0}{@render spacer(band.padGap)}{/if}
    {/if}

    {#each visible as row (row.id)}{@render option(row)}{/each}

    {#if extraRow && !band.extraBefore}
      {#if band.padGap > 0}{@render spacer(band.padGap)}{/if}
      {@render option(extraRow)}
    {/if}
    {#if band.padBottom > 0}{@render spacer(band.padBottom)}{/if}
  </div>
{/if}

{#snippet option(row)}
  <PageRow
    {row}
    total={rows.length}
    selected={row.index === editor.pageIndex}
    focused={row.index === tabStop}
    onpick={() => goToPage(row.index)}
  />
{/snippet}

<!-- The spacers stand in for the rows that are not mounted. They are children
     of a `role="listbox"`, where an unlabelled child is an option with no
     name, so they are marked as presentation and drop out of the tree. -->
{#snippet spacer(height)}
  <div class="pad" role="presentation" style:height="{height}px"></div>
{/snippet}

<style>
  .list { display: flex; flex-direction: column }

  .pad { flex: none }
</style>
