<script>
  /**
   * Setup's last step: each file's own progress, with its own pause, resume
   * or retry. The run belongs to the store, so leaving the setup leaves it
   * running, and Pause all / Resume all are the frame's footer buttons.
   *
   * A pause is `cancelDownload`: the backend keeps the partial file and the
   * next request resumes it with a `Range` header.
   */
  import Icon from '../../icons/Icon.svelte'
  import { hasKey, t } from '../../i18n/index.js'
  import { runtimeNotice } from '../firstlaunch.js'
  import { firstLaunch, pauseFile, resumeFile } from '../firstlaunch.svelte.js'

  const STATUS_KEYS = {
    waiting: 'onboarding.downloads.status.waiting',
    paused: 'onboarding.downloads.status.paused',
    done: 'onboarding.downloads.status.done',
    failed: 'onboarding.downloads.status.failed',
  }

  /** The row button's name and glyph for each state it can be pressed in. */
  const ACTIONS = {
    running: { key: 'onboarding.downloads.pause', icon: 'pause' },
    paused: { key: 'onboarding.downloads.resume', icon: 'play' },
    failed: { key: 'onboarding.downloads.retry', icon: 'refresh' },
  }

  const plan = $derived(firstLaunch.plan)
  // What was already on disk when the queue was built is not a download.
  const queue = $derived(firstLaunch.queue.filter((id) => !plan?.files[id]?.installed))

  /** @param {string} id */
  function nameOf(id) {
    const key = plan?.files[id]?.labelKey
    return key ? t(key) : id
  }

  /** @param {string} id */
  function percentOf(id) {
    const progress = firstLaunch.progress[id]
    return progress?.total ? Math.min(100, Math.floor((progress.downloaded / progress.total) * 100)) : 0
  }

  /** @param {string} id */
  function statusText(id) {
    const status = firstLaunch.status[id] ?? 'waiting'
    if (status === 'active') {
      return firstLaunch.progress[id]?.total
        ? t('onboarding.downloads.status.active', { percent: percentOf(id) })
        : t('onboarding.downloads.status.starting')
    }
    return t(STATUS_KEYS[status])
  }

  /**
   * The failure's own sentence when the backend has one (the runtime's
   * refusals, such as too little disk space), else what it said.
   *
   * @param {string} message
   */
  function errorText(message) {
    const notice = runtimeNotice(message)
    return notice && hasKey(notice.key) ? t(notice.key, notice.params) : message
  }
</script>

{#if queue.length === 0}
  <p class="lead">{t('onboarding.downloads.empty')}</p>
{:else}
  <ul class="list">
    {#each queue as id (id)}
      {@const status = firstLaunch.status[id] ?? 'waiting'}
      {@const name = nameOf(id)}
      <li class="item" data-status={status}>
        <div class="line">
          <span class="name">{name}</span>
          <span class="status">{statusText(id)}</span>
          {#if status === 'done'}
            <span class="icon done" aria-hidden="true"><Icon name="check" size={14} /></span>
          {:else if id !== 'samTs' || status !== 'active'}
            <!-- One button through pause, resume and retry, so pressing it
                 keeps focus where it is instead of dropping it to the body. -->
            {@const action = ACTIONS[status === 'paused' || status === 'failed' ? status : 'running']}
            <button
              type="button"
              class="icon"
              aria-label={t(action.key, { name })}
              title={t(action.key, { name })}
              onclick={() => (status === 'paused' || status === 'failed' ? resumeFile(id) : pauseFile(id))}
            ><Icon name={action.icon} size={14} /></button>
          {/if}
        </div>
        <div class="bar" aria-hidden="true"><span style:transform="scaleX({status === 'done' ? 1 : percentOf(id) / 100})"></span></div>
        {#if firstLaunch.errors[id]}<p class="alert" role="alert">{errorText(firstLaunch.errors[id])}</p>{/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .list { list-style: none; margin: 0; padding: 0 }
  .item { padding: var(--s-4) 0; border-top: 1px solid var(--line) }
  .line { display: flex; align-items: center; gap: var(--s-5) }
  .name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap }
  .status { color: var(--t2); font-size: 11.5px; font-variant-numeric: tabular-nums }
  .item[data-status='failed'] .status { color: var(--warn) }
  .icon {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    padding: 0;
    border: none;
    border-radius: var(--r-sm);
    background: none;
    color: var(--t2);
    cursor: pointer;
  }
  .icon:hover { background: var(--accent-soft); color: var(--text) }
  .icon.done { color: var(--accent); cursor: default }
  .icon.done:hover { background: none }
  .bar {
    height: 2px;
    margin-top: var(--s-3);
    overflow: hidden;
    border-radius: var(--r-pill);
    background: var(--line);
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    transform-origin: left;
    transition: transform var(--dur-slow) var(--ease);
  }
  .item[data-status='paused'] .bar span { background: var(--t3) }
</style>
