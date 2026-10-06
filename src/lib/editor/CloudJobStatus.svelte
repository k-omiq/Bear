<script>
  /**
   * The cloud renders in flight, bottom-left above the loaded-models panel.
   *
   * One row per render: what it is doing now, how long it has taken, the page
   * it is for, and Cancel from the first moment (the job is shown before its
   * first event, `cloudflow.svelte.js#runCloudJob`). When no render has
   * finished recently the row also says that a first run can take 1 to 3
   * minutes, because a GPU may be starting, which is the wait that otherwise
   * reads as a hang.
   *
   * It never blocks anything and draws nothing when nothing is running. The
   * render's ending is a notice, not a row: `state/cloud.svelte.js` removes
   * the job and says how it ended, once.
   */
  import Icon from '../icons/Icon.svelte'
  import { cloud, cloudPhaseKey, cancelCloudJob } from '../state/cloud.svelte.js'
  import { t } from '../i18n/index.js'

  /** Phases past which a cold start is no longer the wait. */
  const WARM_PHASES = new Set(['running', 'downloading', 'compositing'])

  let now = $state(Date.now())

  // One tick a second, only while something is running.
  $effect(() => {
    if (cloud.jobs.length === 0) return
    now = Date.now()
    const timer = setInterval(() => {
      now = Date.now()
    }, 1000)
    return () => clearInterval(timer)
  })

  /**
   * @param {number} startedAt
   * @returns {string} minutes and seconds, `m:ss`
   */
  function elapsed(startedAt) {
    const seconds = Math.max(0, Math.floor((now - startedAt) / 1000))
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
  }
</script>

{#if cloud.jobs.length > 0}
  <section class="tab" aria-label={t('notice.cloud.job.title')}>
    <header class="head">
      <span class="mark"><Icon name="cloud" size={12} /></span>
      <h2 class="title">{t('notice.cloud.job.title')}</h2>
    </header>
    <ul class="rows">
      {#each cloud.jobs as job (job.attemptId)}
        <li class="row">
          <div class="line">
            <span class="phase" aria-live="polite">{t(cloudPhaseKey(job.phase))}</span>
            <span class="time" title={t('notice.cloud.job.elapsed')}>{elapsed(job.startedAt)}</span>
          </div>
          <div class="line">
            <span class="where">
              {typeof job.pageIndex === 'number'
                ? t('notice.cloud.job.page', { page: job.pageIndex + 1 })
                : t('notice.cloud.job.region')}
            </span>
            <!-- aria-disabled, not disabled: WebKit drops focus from a control
                 that turns disabled under it, and the render can take a while
                 to stop. A second press is ignored by cancelCloudJob. -->
            <button
              type="button"
              class="cancel"
              aria-disabled={job.cancelling}
              onclick={() => cancelCloudJob(job.attemptId)}
            >
              {job.cancelling ? t('notice.cloud.job.cancelling') : t('notice.cloud.job.cancel')}
            </button>
          </div>
          {#if job.cold && !WARM_PHASES.has(job.phase)}
            <p class="hint">{t('notice.cloud.job.firstRun')}</p>
          {/if}
        </li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .tab {
    width: 230px;
    max-width: calc(100vw - 28px);
    padding: 8px 9px 7px;
    border-radius: var(--r-md);
    background: var(--surface);
    box-shadow: var(--edge);
    animation: mcIn var(--dur) var(--ease);
  }

  .head {
    display: flex;
    gap: var(--s-3);
    align-items: center;
    margin-bottom: 5px;
  }
  .mark { display: flex; flex: none; color: var(--t3) }
  .title {
    margin: 0;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--t3);
  }

  .rows {
    display: grid;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .row { display: grid; gap: 2px }

  .line {
    display: flex;
    gap: var(--s-3);
    align-items: center;
    justify-content: space-between;
    min-height: 18px;
  }

  .phase {
    min-width: 0;
    overflow: hidden;
    font-size: 11px;
    line-height: 1.45;
    color: var(--text);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Tabular figures so the clock does not jitter as it counts. */
  .time {
    flex: none;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    color: var(--t2);
  }

  .where {
    font-size: 10px;
    color: var(--t3);
  }

  .cancel {
    flex: none;
    height: 18px;
    margin-right: -4px;
    padding: 0 5px;
    border: none;
    border-radius: var(--r-xs);
    background: transparent;
    color: var(--t2);
    font: inherit;
    font-size: 11px;
    cursor: pointer;
    transition: color var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease);
  }
  .cancel:hover:not([aria-disabled='true']) {
    background: var(--accent-soft);
    color: var(--text);
  }
  .cancel[aria-disabled='true'] {
    cursor: default;
    color: var(--t3);
  }

  .hint {
    margin: 2px 0 0;
    font-size: 10px;
    line-height: 1.4;
    color: var(--t3);
  }
</style>
