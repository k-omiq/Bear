<script>
  import { t } from '../i18n/index.js'
  import { ContextMenu, IconButton, Menu } from '../ui/index.js'
  import { menuPoint } from '../editor/gesture.js'
  import { chapterMenuItems, runChapterAction } from './actions.js'
  import StatusMark from './StatusMark.svelte'
  import { runningJobFor } from '../state/jobs.svelte.js'

  /**
   * One chapter. Everything a scanlator needs before deciding to open it:
   * where it stands, how big it is, what is waiting in it, and when they last
   * touched it. The row is one button - the whole thing opens the chapter.
   *
   * The menu sits **beside** that button rather than inside it, the way
   * `ProjectCard`'s does, because a button inside a button is not a control the
   * keyboard or a screen reader can reach. It is tabbable only while this row
   * is the list's active item, so a chapter list costs one Tab to leave.
   *
   * A chapter that was denoised says so in its sub-line, a fact like the
   * others, and its history is Denoise history in that menu.
   *
   * A chapter with a job running on it (`state/jobs.svelte.js`) leads its
   * sub-line with what the job is doing and how far it is, with the blinking
   * mark `RunIndicator` uses for "working now".
   *
   * @type {{
   *   chapter: import('../api/backend.js').ApiChapter,
   *   progress: import('../model/progress.js').Progress,
   *   mode: 'single'|'longstrip',
   *   interruptedAt: number|null,
   *   active: boolean,
   *   project: import('../api/backend.js').ApiProject,
   *   onopen: (chapter: import('../api/backend.js').ApiChapter) => void,
   * }}
   */
  let { chapter, progress, mode, interruptedAt, active, project, onopen } = $props()

  const menuLabel = $derived(t('home.action.chapterMenu', { name: chapter.name }))

  /**
   * The same items on the secondary press, at the pointer: `chapterMenuItems`
   * cut into sections at its separators, which is the shape `ContextMenu`
   * draws. `{x, y}` in client pixels while open, else null.
   *
   * @type {{x: number, y: number}|null}
   */
  let context = $state(null)

  const sections = $derived.by(() => {
    /** @type {Array<{id: string, items: Array<any>}>} */
    const list = [{ id: 'section-0', items: [] }]
    for (const item of chapterMenuItems(chapter)) {
      if (item.separator) list.push({ id: `section-${list.length}`, items: [] })
      else list[list.length - 1].items.push(item)
    }
    return list.filter((section) => section.items.length > 0)
  })

  /**
   * A right-click anywhere on the row. The native menu is replaced, not
   * joined: WebKit's own offers Reload, which here would throw the library
   * away.
   *
   * @param {MouseEvent & {currentTarget: HTMLElement}} event
   */
  function oncontextmenu(event) {
    event.preventDefault()
    context = menuPoint(event, event.currentTarget.getBoundingClientRect())
  }

  /**
   * Shift+F10 and the context-menu key, handled as keys rather than left to
   * the browser: WebKit on macOS raises no `contextmenu` event for either.
   * The menu then opens over the middle of the row, as a keyboard-raised
   * menu does anywhere else in the app (`menuPoint`).
   *
   * @param {KeyboardEvent & {currentTarget: HTMLElement}} event
   */
  function onkeydown(event) {
    const shiftF10 = event.key === 'F10' && event.shiftKey && !event.altKey && !event.ctrlKey && !event.metaKey
    if (!shiftF10 && event.key !== 'ContextMenu') return
    event.preventDefault()
    event.stopPropagation()
    context = menuPoint({}, event.currentTarget.getBoundingClientRect())
  }

  const skipped = $derived(chapter.pages.filter((page) => page.status === 'skipped').length)

  const ACTIVE_KEYS = {
    detect: 'jobs.active.detect',
    clean: 'jobs.active.clean',
    cloudClean: 'jobs.active.cloudClean',
    denoise: 'jobs.active.denoise',
    cloudDenoise: 'jobs.active.cloudDenoise',
  }
  const job = $derived(runningJobFor(chapter.id))
  const jobLine = $derived.by(() => {
    if (!job) return ''
    const kindKey = ACTIVE_KEYS[job.kind]
    return job.total > 0
      ? t('jobs.row.progress', { kindKey, page: Math.min(job.total, job.done + 1), total: job.total })
      : t('jobs.row.starting', { kindKey })
  })

  /** The sub-line, as independent facts - never one concatenated sentence. */
  const facts = $derived.by(() => {
    const list = [
      mode === 'longstrip'
        ? t('home.chapter.positions', { count: progress.totalPages })
        : t('home.chapter.pages', { count: progress.totalPages }),
    ]
    if (progress.regionsNeedingReview > 0) {
      list.push(t('home.chapter.needReview', { count: progress.regionsNeedingReview }))
    }
    if (skipped > 0) list.push(t('home.chapter.skipped', { count: skipped }))
    if (interruptedAt !== null) {
      list.push(t('home.chapter.interrupted', { page: interruptedAt + 1 }))
    }
    if (chapter.denoiseHistory) {
      list.push(t(chapter.denoiseHistory.taken ? 'home.chapter.denoisedTaken' : 'home.chapter.denoised'))
    }
    return list
  })
</script>

<li class="row" {oncontextmenu}>
  <button
    type="button"
    class="hit"
    data-roving-item
    tabindex={active ? 0 : -1}
    aria-keyshortcuts="Shift+F10"
    onclick={() => onopen(chapter)}
    {onkeydown}
  >
    <StatusMark status={progress.status} statusKey={progress.statusKey} variant="inline" />

    <span class="number">{t('home.chapter.number', { number: chapter.number })}</span>

    <span class="main">
      <span class="title" title={chapter.name}>{chapter.name}</span>
      <span class="facts">
        {#if job}<span class="job" data-running={job.kind}><span class="dot" aria-hidden="true">●</span>{jobLine}</span>{/if}
        {#each facts as fact (fact)}<span>{fact}</span>{/each}
      </span>
    </span>

    <!-- The word is already in the row once, in the mark's visually-hidden
         span, which is the copy that survives below 760px where this column is
         dropped. This one is the sighted reader's, and repeats it. -->
    <span class="status" aria-hidden="true">{t(progress.statusKey)}</span>
    <span class="cleaned">
      {t('home.chapter.cleaned', {
        cleaned: progress.pagesCleaned,
        total: progress.totalPages,
      })}
    </span>
    <span class="when">{t(chapter.lastOpened.key, chapter.lastOpened.params)}</span>
  </button>

  <div class="more">
    <Menu
      items={chapterMenuItems(chapter)}
      onselect={(id) => runChapterAction(id, project, chapter)}
      label={menuLabel}
      align="end"
    >
      {#snippet trigger({ toggle, triggerProps })}
        <IconButton
          icon="more-horizontal"
          label={menuLabel}
          size={26}
          tabindex={active ? 0 : -1}
          onclick={toggle}
          {...triggerProps}
        />
      {/snippet}
    </Menu>
  </div>

  {#if context}
    <ContextMenu
      x={context.x}
      y={context.y}
      label={menuLabel}
      {sections}
      onselect={(id) => runChapterAction(id, project, chapter)}
      onclose={() => (context = null)}
    />
  {/if}
</li>

<style>
  /* The row is a button and the menu is beside it, so the `li` is the thing
     that lays them out - and the menu only appears on hover or focus, which is
     what keeps a destructive control out of a list the user is reading. */
  .row {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    border-bottom: 1px solid var(--line);
  }

  .more { flex: none; opacity: 0; transition: opacity var(--dur-fast) var(--ease) }
  .row:hover .more,
  .more:focus-within { opacity: 1 }

  .hit {
    display: flex;
    align-items: center;
    gap: var(--s-6);
    width: 100%;
    padding: 18px 6px;
    border: none;
    border-radius: var(--r-sm);
    background: transparent;
    text-align: start;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease);
  }
  .hit:hover { background: var(--accent-soft) }

  .number {
    width: 72px;
    flex: none;
    overflow: hidden;
    font-size: 13px;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text);
  }

  .main { flex: 1; min-width: 0 }
  .title {
    display: block;
    overflow: hidden;
    font-size: 13px;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text);
  }
  .facts {
    display: block;
    margin-top: 4px;
    overflow: hidden;
    font-size: 11px;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--t3);
  }
  .facts span + span::before { content: '·'; margin: 0 5px }
  .job { color: var(--t2) }
  .dot {
    margin-right: 4px;
    font-size: 8px;
    animation: mcBlink 1.4s ease-in-out infinite;
  }

  .status,
  .cleaned,
  .when {
    flex: none;
    font-size: 11.5px;
    text-align: end;
    white-space: nowrap;
  }
  .status { width: 120px; color: var(--t2) }
  .cleaned { width: 130px; color: var(--t3) }
  .when { width: 104px; color: var(--t3) }

  /* Narrow windows drop the columns that repeat information the row already
     carries - the mark keeps the status, the sub-line keeps the size. */
  @media (max-width: 900px) { .when { display: none } }
  @media (max-width: 760px) { .status { display: none } }
</style>
