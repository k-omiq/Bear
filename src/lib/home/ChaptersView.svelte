<script>
  import { Button, Empty, SectionLabel } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import ChapterRow from './ChapterRow.svelte'
  import { focusItem, nextIndex } from './roving.js'
  import { interruptedChapter } from './library.svelte.js'
  import { openNewChapter } from './actions.js'
  import { chapterProgress } from '../model/progress.js'
  import { compareChapterNumbers } from './chapternumber.js'
  import { goLibrary, openChapter } from '../state/app.svelte.js'

  /**
   * Level two: one project's chapters - `← Projects`, the name, the meta line,
   * `New chapter`, and the table.
   *
   * The header carries nothing else. It had a resume button and a project menu;
   * both were standing weight the design file does not have, and both are
   * still reachable - the interrupted chapter says `Interrupted at page n` on
   * its own row and resumes from the project's card, and the project verbs are
   * on the card's context menu one level up.
   *
   * @type {{ project: import('../api/backend.js').ApiProject }}
   */
  let { project } = $props()

  // Highest number first, as the list has always read for chapters added in
  // order, and natural: a split 12.5 sits between 13 and 12
  // (`chapternumber.js`). Equal numbers keep the backend's order.
  const rows = $derived(
    [...project.chapters]
      .sort((a, b) => compareChapterNumbers(b.number, a.number))
      .map((chapter) => ({ chapter, progress: chapterProgress(chapter) })),
  )
  const totals = $derived(rows.reduce((sum, row) => sum + row.progress.totalPages, 0))
  const interrupted = $derived(interruptedChapter(project))

  const meta = $derived(
    t('home.project.meta', {
      chapters: project.chapters.length,
      pages: totals,
      modeKey: `project.mode.${project.mode}`,
      path: project.sourcePath,
    }),
  )

  /** @type {HTMLElement | undefined} */
  let list = $state()
  let active = $state(0)

  $effect(() => {
    if (active > rows.length - 1) active = Math.max(0, rows.length - 1)
  })

  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    if (event.metaKey || event.ctrlKey || event.altKey) return
    const next = nextIndex(event.key, {
      index: active,
      count: rows.length,
      orientation: 'list',
    })
    if (next === null) return
    event.preventDefault()
    active = next
    focusItem(list, next)
  }

  /** @param {FocusEvent} event */
  function onfocusin(event) {
    const item = /** @type {HTMLElement} */ (event.target)?.closest?.('[data-roving-item]')
    if (!item) return
    const items = [...(list?.querySelectorAll('[data-roving-item]') ?? [])]
    const index = items.indexOf(item)
    if (index >= 0) active = index
  }
</script>

<section class="chapters">
  <Button variant="plain" size="lg" onclick={goLibrary}>
    {t('home.action.backToProjects')}
  </Button>

  <div class="head">
    <div class="titles">
      <!-- h2, not h1: the wordmark in `HomeHeader` is the page's only h1, and
           the section label below is the h3 under this. -->
      <h2 class="name" title={project.name}>{project.name}</h2>
      <p class="meta" title={meta}>{meta}</p>
    </div>

    <div class="tools">
      <Button size="xl" onclick={() => openNewChapter(project.id)}>
        {t('home.action.newChapter')}
      </Button>
    </div>
  </div>

  <div class="label">
    <SectionLabel text={t('home.section.chapters')} as="h3" />
  </div>

  {#if rows.length === 0}
    <Empty title={t('home.empty.chaptersTitle')} />
  {:else}
    <!-- Handlers on the list, not on each row: the rows are the buttons that
         receive the key, and the list is never focused itself. -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <ul
      bind:this={list}
      class="rows"
      aria-label={t('home.section.chapters')}
      onkeydown={onkeydown}
      onfocusin={onfocusin}
    >
      {#each rows as row, i (row.chapter.id)}
        <ChapterRow
          chapter={row.chapter}
          progress={row.progress}
          mode={project.mode}
          interruptedAt={interrupted?.chapter.id === row.chapter.id
            ? interrupted.pageIndex
            : null}
          active={i === active}
          {project}
          onopen={(chapter) => openChapter(project.id, chapter.id)}
        />
      {/each}
    </ul>
  {/if}
</section>

<style>
  /* The design file's drop from the header to the `← Projects` link. */
  .chapters { padding-top: 96px }

  .head {
    display: flex;
    align-items: flex-end;
    gap: var(--s-7);
    margin-top: 22px;
  }
  .titles { flex: 1; min-width: 0 }

  .name {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
    margin: 0;
    font-size: 27px;
    font-weight: 500;
    letter-spacing: -.02em;
    line-height: 1.1;
  }
  .meta {
    margin: 9px 0 0;
    overflow: hidden;
    font-size: 11.5px;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--t3);
  }

  .tools { display: flex; flex: none; align-items: center; gap: var(--s-3) }

  .label { margin: 56px 0 4px }

  .rows {
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--line);
    list-style: none;
  }

  @media (max-width: 700px) {
    .head { flex-direction: column; align-items: stretch; gap: var(--s-5) }
    .tools { flex-wrap: wrap }
  }
</style>
