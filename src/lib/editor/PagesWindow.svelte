<script>
  /**
   * The Pages window. The shell is `FloatingWindow`; the body is `PageList`,
   * which Task 8 owns (see that file's header for the contract).
   */
  import { editor, pages, pageCount } from '../state/editor.svelte.js'
  import { t } from '../i18n/index.js'
  import FloatingWindow from './FloatingWindow.svelte'
  import RunIndicator from './RunIndicator.svelte'
  import PageList from './PageList.svelte'
  import { IconButton } from '../ui/index.js'
  import { openChapterPages } from '../home/chapterpages.svelte.js'

  const total = $derived(pageCount())
  // Cleaned, not merely touched: a page still in the queue or skipped by the
  // script gate is not one the user is finished with.
  const done = $derived(pages().filter((page) => page.status === 'cleaned').length)

  // The header is 248px wide and holds a title, this meta, the run indicator
  // and two buttons. With a run up there is room for about 57px of meta, and
  // `23 of 24 cleaned` needs 85 - it ellipsised to `23 of …`, which is not a
  // number at all. The run indicator is the more urgent of the two counts
  // while a run is up, and every row still carries its own ratio, so the
  // standing total stands down until the run ends.
  const meta = $derived(
    editor.run.active ? '' : t('editor.meta.pagesCleaned', { done, total }),
  )
</script>

<FloatingWindow id="pages" title={t('editor.panel.pages')} {meta} pad="5px 7px 9px">
  {#snippet headerExtra()}
    <RunIndicator />
    <!-- The chapter's Pages screen: add, remove and move pages. -->
    {#if editor.project && editor.chapter}
      <IconButton
        icon="pages"
        label={t('home.action.managePages')}
        size={22}
        iconSize={13}
        onclick={() => editor.project && editor.chapter && openChapterPages({
          projectId: editor.project.id,
          chapterId: editor.chapter.id,
          number: editor.chapter.number,
          name: editor.chapter.name,
        })}
      />
    {/if}
  {/snippet}
  <PageList />
</FloatingWindow>
