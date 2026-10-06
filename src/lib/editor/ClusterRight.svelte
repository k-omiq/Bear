<script>
  /**
   * Top right: how the page is being looked at, then what can be done with it.
   *
   * Export is drawn in the active icon-button state - it is the one action in
   * the chrome that finishes the job, and the design file gives it the filled
   * treatment for that reason. Settings opens the Settings dialog directly,
   * which is what the `,` in its tooltip does.
   *
   * Text-shaped review is the optional all-text mode (M6), so its button is
   * feature-gated on the session's text policy rather than drawn for every
   * chapter: with the legacy default the pill holds only Settings and Export.
   * An always-visible icon marked "optional" would need a word this 34px icon
   * pill has no room for, and a tooltip is not a visible marking.
   *
   * The review is also where a page is analyzed on the cloud GPU, which does
   * not depend on the text policy. So with a usable cloud GPU the button is
   * there under the legacy policy too, as the cloud entry: a cloud icon, the
   * cloud action's name, and the review opened with its cloud choice made.
   *
   * The review opens on the chapter the editor holds and the page in view, as
   * a run from the Text cleanup panel (`startRun`) does. Not the
   * route's: that names the chapter asked for, which differs from the one on
   * screen while a switch loads or after an open that failed, and the review
   * lists its pages from the editor's chapter. Held while there is none.
   *
   * First in the row, while there are any, is Jobs: the runs and denoises
   * going on in the background, this chapter's and others'
   * (`shell/JobsIndicator.svelte`).
   */
  import { pushModal } from '../state/app.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { cloudUsable } from '../state/cloud.svelte.js'
  import { hasJobs } from '../state/jobs.svelte.js'
  import JobsIndicator from '../shell/JobsIndicator.svelte'
  import { IconButton } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import Pill from './Pill.svelte'
  import ViewControls from './ViewControls.svelte'

  const allText = $derived(session.textPolicy === 'all_text')

  function openReview() {
    const chapter = editor.chapter
    if (!chapter || editor.loading) return
    pushModal({ kind: 'workflowReview', props: { chapterId: chapter.id, pageIndex: editor.pageIndex, cloud: !allText } })
  }
</script>

<div class="cluster" role="group" aria-label={t('editor.region.view')}>
  {#if hasJobs()}
    <Pill height={34} gap={4} pad="0 4px">
      <JobsIndicator variant="editor" />
    </Pill>
  {/if}

  <Pill height={34} gap={7} pad="0 9px">
    <ViewControls />
  </Pill>

  <Pill height={34} gap={4} pad="0 5px">
    {#if allText || cloudUsable()}
      <IconButton
        icon={allText ? 'search' : 'cloud'}
        label={t(allText ? 'editor.action.textShapeReview' : 'cloud.analysis.entry.action')}
        disabled={!editor.chapter || editor.loading}
        onclick={openReview}
      />
    {/if}
    <IconButton
      icon="settings"
      label={t('editor.action.settings')}
      shortcut=","
      onclick={() => pushModal({ kind: 'settings' })}
    />
    <IconButton
      icon="export"
      label={t('editor.action.export')}
      shortcut="E"
      active
      onclick={() => pushModal({ kind: 'export' })}
    />
  </Pill>
</div>

<style>
  .cluster {
    position: absolute;
    right: 16px;
    top: 16px;
    z-index: 40;
    display: flex;
    align-items: center;
    gap: var(--s-3);
  }
</style>
