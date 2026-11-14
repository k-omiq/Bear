<script>
  import { untrack } from 'svelte'
  import { Button, Disclosure, Empty, IconButton, Modal, Segmented } from '../../ui/index.js'
  import Icon from '../../icons/Icon.svelte'
  import { t } from '../../i18n/index.js'
  import { closeModal } from '../../state/app.svelte.js'
  import { chooseImages, listenFileDrops } from '../../api/folder.js'
  import { isTauri } from '../../api/tauri.js'
  import {
    addPages,
    movePages,
    readChapterPages,
    removePages,
    restorePages,
  } from '../chapterpages.svelte.js'
  import {
    insertRefusal,
    moveRefusal,
    orderMovingTo,
    orderStepping,
    pageState,
    removalLoss,
    removeRefusal,
  } from '../pagerules.js'
  import PageThumb from '../PageThumb.svelte'

  /**
   * The Pages screen: one chapter's pages, over the whole window.
   *
   * Reached from the chapter row's menu on Home and from the editor's Pages
   * window. It shows every page in order with its number and state, and it
   * adds pages (the file chooser, or files and folders dropped anywhere on the
   * screen, placed at the end or around the selection), removes the selected
   * pages after an inline confirmation, moves them (drag, or the two arrow
   * buttons, or Alt+arrow) and puts removed pages back.
   *
   * **What it may do is the backend's rule** (`../pagerules.js`): a page with
   * work keeps its place, a longstrip with work only grows at its end. A
   * button the rule refuses is disabled and the line under the toolbar says
   * why, so a refusal is never a surprise.
   *
   * The confirmation is inline rather than a dialog of its own: the modal host
   * mounts one dialog at a time, and a second one would unmount this screen
   * and its selection with it.
   *
   * @type {{ spec: import('../../state/app.svelte.js').ModalSpec }}
   */
  let { spec } = $props()

  const given = untrack(() => /** @type {any} */ (spec.props) ?? {})
  const projectId = /** @type {string} */ (given.projectId)
  const chapterId = /** @type {string} */ (given.chapterId)

  /** @type {import('../../api/backend.js').ChapterPages|null} */
  let listing = $state(null)
  let loading = $state(true)
  let working = $state(false)
  /** Positions, in the current listing. @type {number[]} */
  let selected = $state([])
  /** The page that keyboard focus sits on. */
  let active = $state(0)
  /** Where a selection range starts, for Shift-click. */
  let anchor = $state(-1)
  /** @type {'end'|'before'|'after'} */
  let place = $state('end')
  let confirming = $state(false)
  let removedOpen = $state(false)
  let dropping = $state(false)
  /** The position a drag would put the selection before, or -1. */
  let dropAt = $state(-1)

  const pages = $derived(listing?.pages ?? [])
  const count = $derived(pages.length)
  const ordered = $derived([...selected].sort((a, b) => a - b))

  $effect(() => {
    untrack(() => load())
  })

  async function load() {
    loading = true
    listing = await readChapterPages(chapterId)
    loading = false
  }

  /** @param {import('../../api/backend.js').ChapterPages|null} next */
  function take(next) {
    if (!next) return false
    listing = next
    selected = selected.filter((i) => i < next.pages.length)
    active = Math.min(active, Math.max(0, next.pages.length - 1))
    return true
  }

  /* ---------- where added pages go ---------- */

  const insertAt = $derived(
    place === 'before' && ordered.length
      ? ordered[0]
      : place === 'after' && ordered.length
        ? ordered[ordered.length - 1] + 1
        : count,
  )
  const addWhy = $derived(listing ? insertRefusal(listing, insertAt) : null)
  const removeWhy = $derived(listing && ordered.length ? removeRefusal(listing, ordered) : null)
  const earlier = $derived(orderStepping(count, ordered, -1))
  const later = $derived(orderStepping(count, ordered, 1))
  const earlierWhy = $derived(listing && earlier ? moveRefusal(listing, earlier) : null)
  const laterWhy = $derived(listing && later ? moveRefusal(listing, later) : null)

  /**
   * The one reason on screen: why Remove, or adding where the placement
   * says, is refused for this selection. A move refused says so while the
   * page is being dragged (`dropWhy`); its arrow buttons are only disabled.
   */
  const why = $derived.by(() => {
    if (!listing) return null
    if (listing.busy) return { key: 'home.pagesScreen.why.busy', params: {} }
    return removeWhy ?? addWhy
  })

  const placeOptions = $derived([
    { value: 'end', label: t('home.pagesScreen.place.end') },
    { value: 'before', label: t('home.pagesScreen.place.before'), disabled: ordered.length === 0 },
    { value: 'after', label: t('home.pagesScreen.place.after'), disabled: ordered.length === 0 },
  ])

  $effect(() => {
    if (ordered.length === 0 && place !== 'end') place = 'end'
  })

  /* ---------- edits ---------- */

  /** @param {string[]} paths */
  async function add(paths) {
    if (!paths.length || working || addWhy) return
    working = true
    const at = insertAt === count ? undefined : insertAt
    const added = take(await addPages({ projectId, chapterId, paths, at }))
    working = false
    if (added && at !== undefined) selected = []
  }

  async function pick() {
    const paths = await chooseImages({ title: t('home.pagesScreen.chooserTitle') })
    await add(paths)
  }

  function askRemove() {
    if (!ordered.length || removeWhy || working) return
    confirming = true
  }

  async function remove() {
    confirming = false
    if (!ordered.length || removeWhy) return
    working = true
    const gone = take(await removePages({ projectId, chapterId, indices: ordered }))
    working = false
    if (gone) selected = []
  }

  /** @param {number[]|null} order */
  async function move(order) {
    if (!order || working || !listing || moveRefusal(listing, order)) return
    const moving = ordered.map((i) => order.indexOf(i))
    working = true
    const moved = take(await movePages({ projectId, chapterId, order }))
    working = false
    if (moved) {
      selected = moving
      active = moving[0] ?? active
    }
  }

  /** @param {number} sourceIndex */
  async function restore(sourceIndex) {
    if (working) return
    working = true
    take(await restorePages({ projectId, chapterId, sources: [sourceIndex] }))
    working = false
  }

  /* ---------- selection ---------- */

  /**
   * @param {number} i
   * @param {MouseEvent|KeyboardEvent} event
   */
  function choose(i, event) {
    confirming = false
    active = i
    if (event.shiftKey && anchor >= 0) {
      const [from, to] = anchor < i ? [anchor, i] : [i, anchor]
      selected = Array.from({ length: to - from + 1 }, (_, k) => from + k)
      return
    }
    anchor = i
    if (event.metaKey || event.ctrlKey) {
      selected = selected.includes(i) ? selected.filter((s) => s !== i) : [...selected, i]
      return
    }
    selected = selected.length === 1 && selected[0] === i ? [] : [i]
  }

  function selectAll() {
    confirming = false
    selected = Array.from({ length: count }, (_, i) => i)
  }

  /** @type {HTMLElement|undefined} */
  let grid = $state()

  /** @param {number} i */
  function focusTile(i) {
    active = i
    /** @type {HTMLElement|null|undefined} */
    const tile = grid?.querySelector(`[data-position="${i}"]`)
    tile?.focus()
  }

  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    if (!count) return
    const step = { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 }[event.key]
    if (step && event.altKey) {
      event.preventDefault()
      move(step < 0 ? earlier : later)
      return
    }
    if (step) {
      event.preventDefault()
      focusTile(Math.max(0, Math.min(count - 1, active + step)))
      return
    }
    if (event.key === 'Home' || event.key === 'End') {
      event.preventDefault()
      focusTile(event.key === 'Home' ? 0 : count - 1)
      return
    }
    if ((event.key === 'a' || event.key === 'A') && (event.metaKey || event.ctrlKey)) {
      event.preventDefault()
      selectAll()
      return
    }
    if (event.key === 'Delete' || event.key === 'Backspace') {
      event.preventDefault()
      askRemove()
    }
  }

  /* ---------- dragging pages to a new place ---------- */

  /** @type {{pointerId: number, x: number, y: number, from: number, moved: boolean}|null} */
  let press = null
  let swallowClick = false

  /**
   * @param {PointerEvent} event
   * @param {number} i
   */
  function onpointerdown(event, i) {
    if (event.button !== 0 || working) return
    press = { pointerId: event.pointerId, x: event.clientX, y: event.clientY, from: i, moved: false }
  }

  /** @param {PointerEvent} event */
  function onpointermove(event) {
    if (!press || event.pointerId !== press.pointerId) return
    if (!press.moved) {
      if (Math.hypot(event.clientX - press.x, event.clientY - press.y) < 6) return
      press.moved = true
      if (!selected.includes(press.from)) selected = [press.from]
      confirming = false
    }
    dropAt = targetAt(event.clientX, event.clientY)
  }

  /** @param {PointerEvent} event */
  function onpointerup(event) {
    if (!press || event.pointerId !== press.pointerId) return
    const dragged = press.moved
    press = null
    const target = dropAt
    dropAt = -1
    if (!dragged) return
    swallowClick = true
    if (target >= 0) move(orderMovingTo(count, ordered, target))
  }

  /**
   * The position the selection would land before, from the tile under the
   * pointer: its own position, or the next one past its middle.
   *
   * @param {number} x
   * @param {number} y
   */
  function targetAt(x, y) {
    const element = /** @type {HTMLElement|null} */ (document.elementFromPoint(x, y))
    const tile = /** @type {HTMLElement|null} */ (element?.closest('[data-position]'))
    if (!tile) return -1
    const at = Number(tile.dataset.position)
    const box = tile.getBoundingClientRect()
    return x > box.left + box.width / 2 ? at + 1 : at
  }

  const dropWhy = $derived(listing && dropAt >= 0 ? moveRefusal(listing, orderMovingTo(count, ordered, dropAt)) : null)

  /* ---------- files dropped on the screen ---------- */

  $effect(() => {
    return listenFileDrops({
      onenter: () => (dropping = true),
      onleave: () => (dropping = false),
      ondrop: (paths) => add(paths),
    })
  })

  /** @param {DragEvent} event */
  function ondragover(event) {
    if (isTauri() || !event.dataTransfer?.types?.includes('Files')) return
    event.preventDefault()
    dropping = true
  }

  /** @param {DragEvent} event */
  function ondrop(event) {
    if (isTauri()) return
    event.preventDefault()
    dropping = false
    const names = [...(event.dataTransfer?.files ?? [])].map((file) => file.name)
    add(names)
  }

  /* ---------- words ---------- */

  const meta = $derived(t('home.pagesScreen.meta', { number: given.number ?? '', name: given.name ?? '' }))

  /** @param {'cleaned'|'masks'|'edited'|'untouched'} state */
  function stateLabel(state) {
    if (state === 'cleaned') return t('home.pagesScreen.state.cleaned')
    if (state === 'masks') return t('home.pagesScreen.state.masks')
    if (state === 'edited') return t('home.pagesScreen.state.edited')
    return t('home.pagesScreen.state.untouched')
  }

  const loss = $derived(listing ? removalLoss(listing, ordered) : { pages: 0, worked: 0 })
</script>

<Modal title={t('home.pagesScreen.title')} {meta} fill onclose={() => closeModal(null)}>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="screen"
    class:dropping
    {ondragover}
    ondragleave={() => (dropping = false)}
    {ondrop}
  >
    <div class="bar">
      <Button variant="primary" onclick={pick} disabled={!listing || working || !!addWhy}>
        {t('home.pagesScreen.add')}
      </Button>
      <span class="place">
        <span class="label" id="pages-place">{t('home.pagesScreen.place.label')}</span>
        <Segmented
          size="chip"
          options={placeOptions}
          value={place}
          onchange={(value) => (place = /** @type {'end'|'before'|'after'} */ (value))}
          labelledBy="pages-place"
        />
      </span>

      <span class="spacer"></span>

      {#if ordered.length}
        <span class="count">{t('home.pagesScreen.selected', { count: ordered.length })}</span>
        <Button variant="plain" onclick={() => (selected = [])}>{t('home.pagesScreen.clear')}</Button>
      {:else}
        <Button variant="plain" onclick={selectAll} disabled={!count}>{t('home.pagesScreen.selectAll')}</Button>
      {/if}
      <IconButton
        icon="chevron-left"
        label={t('home.pagesScreen.moveEarlier')}
        disabled={!earlier || !!earlierWhy || working}
        onclick={() => move(earlier)}
      />
      <IconButton
        icon="chevron-right"
        label={t('home.pagesScreen.moveLater')}
        disabled={!later || !!laterWhy || working}
        onclick={() => move(later)}
      />
      <Button onclick={askRemove} disabled={!ordered.length || !!removeWhy || working}>
        <Icon name="trash" size={13} />
        {ordered.length ? t('home.pagesScreen.remove', { count: ordered.length }) : t('home.action.remove')}
      </Button>
    </div>

    <p class="why" aria-live="polite">
      {#if dropWhy}
        {t(dropWhy.key, dropWhy.params)}
      {:else if why}
        {t(why.key, why.params)}
      {:else}
        {t('home.pagesScreen.hint')}
      {/if}
    </p>

    {#if confirming}
      <div class="confirm" role="alertdialog" aria-labelledby="pages-confirm" aria-describedby="pages-confirm-more">
        <div class="words">
          <p id="pages-confirm" class="ask">{t('home.pagesScreen.confirm.ask', { count: loss.pages })}</p>
          <p id="pages-confirm-more" class="more">
            {loss.worked
              ? t('home.pagesScreen.confirm.loss', { count: loss.worked })
              : t('home.pagesScreen.confirm.noLoss')}
            {t('home.pagesScreen.confirm.kept')}
          </p>
        </div>
        <div class="answers">
          <Button onclick={() => (confirming = false)}>{t('shell.action.cancel')}</Button>
          <Button variant="primary" onclick={remove}>{t('home.pagesScreen.remove', { count: loss.pages })}</Button>
        </div>
      </div>
    {/if}

    <div class="body">
      {#if loading && !listing}
        <Empty title={t('home.pagesScreen.loading')} size="sm" />
      {:else if !listing}
        <Empty title={t('home.pagesScreen.none')} size="sm" />
      {:else}
        <!-- svelte-ignore a11y_interactive_supports_focus -->
        <ul
          bind:this={grid}
          class="grid"
          role="listbox"
          aria-multiselectable="true"
          aria-label={t('home.pagesScreen.title')}
          aria-busy={working}
          {onkeydown}
          onpointermove={onpointermove}
          onpointerup={onpointerup}
          onpointercancel={() => {
            press = null
            dropAt = -1
          }}
        >
          {#each pages as page, i (page.sourceIndex ?? page.id)}
            {@const state = pageState(page, listing.worked[i])}
            {@const pinned = listing.worked[i]}
            <li
              class="cell"
              class:before={dropAt === i}
              class:after={dropAt === i + 1 && i === count - 1}
            >
              <button
                type="button"
                class="tile"
                class:selected={selected.includes(i)}
                role="option"
                aria-selected={selected.includes(i)}
                aria-label={t('home.pagesScreen.tile', { number: i + 1, file: page.file, stateLabel: stateLabel(state) })}
                title={pinned ? t('home.pagesScreen.pinned') : page.file}
                data-position={i}
                tabindex={i === active ? 0 : -1}
                onpointerdown={(event) => onpointerdown(event, i)}
                onclick={(event) => {
                  if (swallowClick) {
                    swallowClick = false
                    return
                  }
                  choose(i, event)
                }}
              >
                <span class="thumb" style:aspect-ratio="{page.width || 2} / {page.height || 3}">
                  <PageThumb {page} />
                </span>
                <span class="caption" aria-hidden="true">
                  <span class="number">{i + 1}</span>
                  <span class="state {state}">{stateLabel(state)}</span>
                  {#if pinned}<span class="lock"><Icon name="lock" size={11} /></span>{/if}
                </span>
                <span class="file" aria-hidden="true">{page.file}</span>
              </button>
            </li>
          {/each}
        </ul>

        {#if listing.removed.length}
          <div class="removed">
            <Disclosure open={removedOpen} ontoggle={(open) => (removedOpen = open)} variant="plain">
              {#snippet summary()}
                {t('home.pagesScreen.removed.title', { count: listing?.removed.length ?? 0 })}
              {/snippet}
              <p class="note">{t('home.pagesScreen.removed.note')}</p>
              <ul class="gone">
                {#each listing.removed as page (page.sourceIndex)}
                  <li>
                    <span class="file">{page.file}</span>
                    <Button size="sm" disabled={working || listing.busy} onclick={() => restore(page.sourceIndex)}>
                      {t('home.pagesScreen.removed.restore')}
                    </Button>
                  </li>
                {/each}
              </ul>
            </Disclosure>
          </div>
        {/if}
      {/if}
    </div>

    {#if dropping}
      <div class="drop" aria-hidden="true">
        <span>{addWhy ? t(addWhy.key, addWhy.params) : t('home.pagesScreen.drop')}</span>
      </div>
    {/if}
  </div>

  {#snippet buttons()}
    <Button variant="primary" onclick={() => closeModal(null)}>{t('home.pagesScreen.done')}</Button>
  {/snippet}
</Modal>

<style>
  .screen {
    position: relative;
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: var(--s-3);
    min-height: 0;
    margin-top: var(--s-4);
  }

  .bar { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s-3) }
  .place { display: flex; align-items: center; gap: var(--s-2) }
  .label,
  .count { font-size: 11.5px; color: var(--t2) }
  .spacer { flex: 1 }

  .why { margin: 0; min-height: 16px; font-size: 11.5px; line-height: 1.4; color: var(--t3) }

  .confirm {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-4);
    padding: var(--s-4) var(--s-5);
    border: 1px solid var(--line2);
    border-radius: var(--r-md);
    background: var(--panel2);
  }
  .words { flex: 1; min-width: 240px }
  .ask { margin: 0; font-size: 12.5px; color: var(--text) }
  .more { margin: var(--s-1) 0 0; font-size: 11.5px; line-height: 1.5; color: var(--t2) }
  .answers { display: flex; gap: var(--s-3) }

  .body { flex: 1; min-height: 0; overflow: auto; padding: 2px }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(118px, 1fr));
    gap: var(--s-5) var(--s-4);
    margin: 0;
    padding: 0;
    list-style: none;
    touch-action: none;
  }
  .cell { position: relative; min-width: 0 }
  /* Where a dragged selection would land: a bar in the gap before the cell. */
  .cell.before::before,
  .cell.after::after {
    content: '';
    position: absolute;
    top: 0;
    bottom: 28px;
    width: 2px;
    border-radius: 1px;
    background: var(--accent);
  }
  .cell.before::before { left: -7px }
  .cell.after::after { right: -7px }

  .tile {
    display: flex;
    flex-direction: column;
    gap: var(--s-1);
    width: 100%;
    padding: var(--s-2);
    border: 1px solid transparent;
    border-radius: var(--r-md);
    background: transparent;
    color: var(--t2);
    text-align: start;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease), border-color var(--dur-fast) var(--ease);
  }
  .tile:hover { background: var(--accent-soft) }
  .tile.selected { border-color: var(--tintline); background: var(--accent-soft); color: var(--text) }
  .tile:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px }

  .thumb {
    position: relative;
    display: block;
    width: 100%;
    max-height: 190px;
    overflow: hidden;
    border-radius: var(--r-xs);
    background: var(--paper);
    box-shadow: var(--edge-soft);
  }

  .caption { display: flex; align-items: center; gap: var(--s-2); font-size: 11.5px }
  .number { font-variant-numeric: tabular-nums; color: var(--text) }
  .state { flex: 1; min-width: 0; overflow: hidden; color: var(--t3); white-space: nowrap; text-overflow: ellipsis }
  .state.cleaned { color: var(--t2) }
  .state.masks,
  .state.edited { color: var(--t2) }
  .lock { display: inline-flex; color: var(--t3) }
  .file {
    overflow: hidden;
    font-size: 10.5px;
    color: var(--t3);
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .removed { margin-top: var(--s-6); padding-top: var(--s-4); border-top: 1px solid var(--line) }
  .note { margin: var(--s-2) 0; font-size: 11.5px; color: var(--t3) }
  .gone { display: flex; flex-direction: column; gap: var(--s-2); margin: 0; padding: 0; list-style: none }
  .gone li { display: flex; align-items: center; justify-content: space-between; gap: var(--s-3); max-width: 520px }

  .drop {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 1.5px dashed var(--tintline);
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--panel) 86%, transparent);
    font-size: 13px;
    color: var(--text);
    pointer-events: none;
  }
</style>
