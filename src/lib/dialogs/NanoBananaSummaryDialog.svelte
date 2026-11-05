<script>
  /**
   * How a Nano Banana batch ended (`nano_banana_job.rs#Report`): how many
   * regions it cleaned, why it stopped early if it did, and the regions it did
   * not clean, counted by reason with the pages they are on.
   *
   * **Send again** starts a new batch of only the regions it did not clean and
   * that are still detections (`nanobananarun.svelte.js#retryNanoBanana`),
   * behind a new cost window. It is offered only while the batch's chapter is
   * open in the editor, since the batch is followed as that chapter's run.
   *
   * The report is read from the jobs store, so a dismissed job closes nothing
   * here: the dialog says what it was opened with.
   */
  import { untrack } from 'svelte'
  import { Button, Modal } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { jobById } from '../state/jobs.svelte.js'
  import { t } from '../i18n/index.js'
  import { retryable, retryNanoBanana } from '../editor/nanobananarun.svelte.js'

  let { spec } = $props()

  const runId = untrack(() => String(spec?.props?.runId ?? ''))
  const job = untrack(() => jobById(runId))
  const chapterId = job?.chapterId ?? null
  /** @type {import('../api/backend.js').NanoBananaReport|null} */
  const report = job?.nanoBanana ?? null

  /** The failed regions grouped by reason, each with its pages, in first-seen order. */
  const reasons = (() => {
    /** @type {Map<string, {count: number, pages: Set<number>}>} */
    const groups = new Map()
    for (const entry of report?.failed ?? []) {
      const group = groups.get(entry.reason) ?? { count: 0, pages: new Set() }
      group.count += 1
      group.pages.add(entry.pageIndex + 1)
      groups.set(entry.reason, group)
    }
    return [...groups].map(([reasonKey, group]) => ({
      reasonKey,
      count: group.count,
      pages: [...group.pages].sort((a, b) => a - b).join(', '),
    }))
  })()
  const again = retryable(report).length
  const here = $derived(chapterId != null && editor.chapter?.id === chapterId && !editor.run.active)

  function retry() {
    closeModal(null)
    void retryNanoBanana(runId)
  }
</script>

<Modal title={t('nanoBanana.job.summaryTitle')} width={420} onclose={() => closeModal(null)}>
  <div class="summary" data-run={runId}>
    {#if report}
      <p class="lead">{t('nanoBanana.job.summaryCleaned', { cleaned: report.cleaned, total: report.total })}</p>
      {#if report.stopped && report.byUser}
        <p class="line">{t('nanoBanana.job.summaryStopped')}</p>
      {:else if report.stopped}
        <p class="line warn">{t('nanoBanana.job.summaryHalted', { reasonKey: report.stopped })}</p>
      {/if}
      {#if reasons.length}
        <ul class="reasons">
          {#each reasons as reason (reason.reasonKey)}
            <li data-reason={reason.reasonKey}>
              <span>{t('nanoBanana.job.summaryReason', { count: reason.count, reasonKey: reason.reasonKey })}</span>
              <span class="pages">{t('nanoBanana.job.summaryPages', { pages: reason.pages })}</span>
            </li>
          {/each}
        </ul>
      {/if}
      {#if report.left?.length}
        <p class="line">{t('nanoBanana.job.summaryLeft', { count: report.left.length })}</p>
      {/if}
      {#if report.gone > 0}
        <p class="line note">{t('nanoBanana.job.summaryGone', { count: report.gone })}</p>
      {/if}
      {#if again > 0 && !here}
        <p class="line note">{t('nanoBanana.job.retryHint')}</p>
      {/if}
    {/if}
  </div>

  {#snippet buttons()}
    <Button onclick={() => closeModal(null)}>{t('shell.action.close')}</Button>
    {#if again > 0 && here}
      <Button variant="primary" data-action="retry" onclick={retry}>{t('nanoBanana.job.retry', { count: again })}</Button>
    {/if}
  {/snippet}
</Modal>

<style>
  .summary { display: grid; gap: var(--s-3); margin-top: var(--s-3) }
  .lead { margin: 0; font-size: 12.5px; line-height: 1.5; color: var(--text) }
  .line { margin: 0; font-size: 12px; line-height: 1.45; color: var(--t2) }
  .warn { color: var(--warn) }
  .note { font-size: 11px; color: var(--t3) }
  .reasons {
    display: grid;
    gap: 6px;
    margin: 0;
    padding: 0 0 0 var(--s-5);
    font-size: 12px;
    line-height: 1.45;
    color: var(--t2);
  }
  .reasons li { display: grid; gap: 1px }
  .pages { font-size: 11px; color: var(--t3); font-variant-numeric: tabular-nums }
</style>
