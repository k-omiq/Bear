<script>
  /**
   * The Jobs button, on Home's header and in the editor's top-right cluster:
   * how many runs and denoises are going, and a panel that lists each with its
   * progress, Stop, Open and, once finished, Dismiss.
   *
   * It draws nothing while the list is empty. The list itself is
   * `state/jobs.svelte.js`'s, so a job started in a dialog or in the editor
   * shows here after that screen is gone.
   *
   * The panel is a `Popover`: not modal, Tab walks through it and out, and it
   * ends on an outside press or Escape. Its buttons act without taking the
   * panel down, since stopping one job is rarely the last thing wanted from
   * it. Stop is `aria-disabled` rather than `disabled` while the stop is on
   * its way: WebKit drops focus from a control that turns disabled under it.
   *
   * The count is announced as it changes (`aria-live`), from a region that is
   * mounted for the life of the button so a job ending is heard.
   *
   * @type {{ variant?: 'home' | 'editor' }}
   */
  import { Button, Popover } from '../ui/index.js'
  import { t } from '../i18n/index.js'
  import { app } from '../state/app.svelte.js'
  import { jobTitle as titleOf } from './jobtitle.js'
  import {
    clearFinishedJobs,
    dismissJob,
    jobFraction,
    jobs,
    openJobChapter,
    runningCount,
    stopJob,
  } from '../state/jobs.svelte.js'

  let { variant = 'home' } = $props()

  const KIND_KEYS = {
    detect: 'jobs.kind.detect',
    clean: 'jobs.kind.clean',
    cloudClean: 'jobs.kind.cloudClean',
    denoise: 'jobs.kind.denoise',
    cloudDenoise: 'jobs.kind.cloudDenoise',
  }

  const running = $derived(runningCount())
  const finished = $derived(jobs.list.length - running)
  const label = $derived(t('jobs.indicator.label', { count: running }))

  /** @param {import('../state/jobs.svelte.js').Job} job */
  function statusOf(job) {
    if (job.status === 'completed') return t('jobs.status.completed')
    if (job.status === 'cancelled') return t('jobs.status.cancelled')
    if (job.status === 'failed') return t('jobs.status.failed')
    if (job.stopping) return t('jobs.status.stopping')
    if (job.total > 0 && job.done < job.total) return t('jobs.status.page', { page: job.done + 1, total: job.total })
    return t('jobs.status.starting')
  }

  /** The editor already shows this chapter: Open would go nowhere. @param {import('../state/jobs.svelte.js').Job} job */
  const openHere = (job) => app.route.name === 'editor' && app.route.chapterId === job.chapterId
</script>

<!-- Mounted whether or not the button is, so the last job ending is heard. -->
<span class="sr" aria-live="polite" aria-atomic="true">{jobs.list.length > 0 ? label : ''}</span>

{#if jobs.list.length > 0}
  <Popover label={t('jobs.title')} align="end" width="300px">
    {#snippet trigger({ toggle, triggerProps })}
      <Button
        size={variant === 'home' ? 'lg' : 'md'}
        variant="soft"
        aria-label={label}
        title={label}
        data-jobs-trigger
        onclick={toggle}
        {...triggerProps}
      >
        {#if running > 0}
          <span class="dot" aria-hidden="true">●</span>{t('jobs.indicator.running', { count: running })}
        {:else}
          {t('jobs.indicator.finished', { count: finished })}
        {/if}
      </Button>
    {/snippet}

    <div class="jobs">
      <h2 class="head">{t('jobs.title')}</h2>
      <ul class="rows">
        {#each jobs.list as job (job.runId)}
          {@const fraction = jobFraction(job)}
          <li class="row" data-job={job.runId} data-status={job.status}>
            <span class="name" title={titleOf(job)}>{titleOf(job)}</span>
            <span class="facts">
              <span>{t(KIND_KEYS[job.kind])}</span>
              <span class:warn={job.status === 'failed'}>{statusOf(job)}</span>
            </span>
            {#if job.status === 'running'}
              <div
                class="bar"
                role="progressbar"
                aria-label={t('jobs.item.progressLabel', { kindKey: KIND_KEYS[job.kind] })}
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow={job.total > 0 ? Math.round(fraction * 100) : undefined}
              ><span style:transform="scaleX({fraction})"></span></div>
            {/if}
            <div class="actions">
              {#if !openHere(job)}
                <button
                  type="button"
                  class="act"
                  aria-label={t('jobs.action.openLabel', { chapter: titleOf(job) })}
                  onclick={() => openJobChapter(job.runId)}
                >{t('jobs.action.open')}</button>
              {/if}
              {#if job.status === 'running'}
                <button
                  type="button"
                  class="act"
                  data-action="stop"
                  aria-disabled={job.stopping}
                  onclick={() => stopJob(job.runId)}
                >{job.stopping ? t('jobs.status.stopping') : t('jobs.action.stop')}</button>
              {:else}
                <button type="button" class="act" data-action="dismiss" onclick={() => dismissJob(job.runId)}>
                  {t('jobs.action.dismiss')}
                </button>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
      {#if finished > 1}
        <div class="foot">
          <button type="button" class="act" onclick={clearFinishedJobs}>{t('jobs.action.clear')}</button>
        </div>
      {/if}
    </div>
  </Popover>
{/if}

<style>
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  /* `RunIndicator`'s "working now" glyph, stopped by reduced motion. */
  .dot {
    font-size: 8px;
    line-height: 1;
    color: var(--t2);
    animation: mcBlink 1.4s ease-in-out infinite;
  }

  .jobs { display: grid; gap: var(--s-3) }

  /* `CloudJobStatus`'s header: the same small caps over the same rows. */
  .head {
    margin: 0 0 0 2px;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: .04em;
    text-transform: uppercase;
    color: var(--t3);
  }

  .rows {
    display: grid;
    gap: 4px;
    max-height: min(360px, 60vh);
    margin: 0;
    padding: 0;
    overflow-y: auto;
    list-style: none;
  }

  .row {
    display: grid;
    gap: 3px;
    padding: 6px 2px 5px;
    border-top: 1px solid var(--line);
  }
  .row:first-child { border-top: none; padding-top: 0 }

  .name {
    min-width: 0;
    overflow: hidden;
    font-size: 12px;
    line-height: 1.4;
    color: var(--text);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .facts {
    min-width: 0;
    overflow: hidden;
    font-size: 11px;
    line-height: 1.4;
    color: var(--t3);
    font-variant-numeric: tabular-nums;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .facts span + span::before { content: '·'; margin: 0 5px }
  .warn { color: var(--warn) }

  /* The denoise dialog's bar (`DenoiseDialog.svelte`), so every run reads the same. */
  .bar { height: 3px; margin: 2px 0; overflow: hidden; border-radius: 2px; background: var(--line) }
  .bar > span {
    display: block;
    height: 100%;
    background: var(--accent);
    transform-origin: left center;
    transition: transform .24s cubic-bezier(.22, 1, .36, 1);
  }

  .actions { display: flex; justify-content: flex-end; gap: 2px; margin-right: -4px }
  .foot { display: flex; justify-content: flex-end; margin-right: -2px; padding-top: 4px; border-top: 1px solid var(--line) }

  /* `CloudJobStatus`'s Cancel: a quiet text control inside a busy row. */
  .act {
    flex: none;
    height: 20px;
    padding: 0 6px;
    border: none;
    border-radius: var(--r-xs);
    background: transparent;
    color: var(--t2);
    font: inherit;
    font-size: 11px;
    cursor: pointer;
    transition: color var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease);
  }
  .act:hover:not([aria-disabled='true']) { background: var(--accent-soft); color: var(--text) }
  .act[aria-disabled='true'] { cursor: default; color: var(--t3) }
</style>
