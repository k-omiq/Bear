<script>
  /**
   * Going back to an earlier released version.
   *
   * Two steps. The list shows the installed version and every older release,
   * newest first, with its date and the start of its notes; a version below
   * the floor is listed but cannot be chosen. Choosing one asks first, and
   * says plainly what an older version cannot do with what this one saved.
   * The install is the update dialog's: the backend checks that version's own
   * signed manifest and hands over the updater plugin's handle, so the
   * download, the signature check, the progress and the restart are the same.
   *
   * Before the install the version being left is remembered
   * (`rollback.js#rememberSkipped`), so the badge at start does not offer it
   * straight back. A failed install puts the old value back.
   *
   * Running jobs are not held, as with a normal update: the install closes the
   * app (on Windows the moment it starts). The confirm step says how many are
   * running, live, before anything is downloaded.
   */
  import { untrack } from 'svelte'
  import { Modal, Button } from '../ui/index.js'
  import { installUpdate, relaunchApp } from '../updater.js'
  import { t, locale } from '../i18n/index.js'
  import { getBackend } from '../api/backend.js'
  import { openExternal } from '../api/external.js'
  import { runningJobs } from '../state/jobs.svelte.js'
  import {
    ROLLBACK_ERROR_KEYS,
    asUpdate,
    displayVersion,
    offersPage,
    rememberSkipped,
    restoreSkipped,
    rollbackErrorCode,
    rollbackRows,
    shortNotes,
  } from '../rollback.js'

  /** @type {{ open: boolean }} */
  let { open = $bindable(false) } = $props()

  /** @type {import('../rollback.js').RollbackCatalog | null} */
  let catalog = $state(null)
  let loading = $state(false)
  let listError = $state('')
  /** @type {import('../rollback.js').RollbackRow | null} */
  let chosen = $state(null)
  let phase = $state(/** @type {'list' | 'confirm' | 'installing' | 'installed'} */ ('list'))
  let percent = $state(/** @type {number | null} */ (null))
  let error = $state('')
  let errorCode = $state('')

  const rows = $derived(catalog ? rollbackRows(catalog) : [])
  const choosable = $derived(rows.some((row) => row.state === 'available' || row.state === 'pageOnly'))
  const jobsRunning = $derived(runningJobs().length)
  const current = $derived(displayVersion(catalog?.currentVersion ?? ''))
  const target = $derived(displayVersion(chosen?.version ?? ''))

  /**
   * Whether this opening has asked for the list. Not `$state`: the effect
   * must not re-run when it changes. The list is asked for once per opening;
   * a failure stays on screen until Retry, so an offline or rate-limited
   * machine does not ask the releases list again and again.
   */
  let asked = false

  $effect(() => {
    if (open) {
      if (!asked) {
        asked = true
        if (!untrack(() => catalog)) untrack(load)
      }
    } else {
      asked = false
      reset()
    }
  })

  function reset() {
    chosen = null
    phase = 'list'
    error = ''
    errorCode = ''
    percent = null
  }

  async function load() {
    if (loading) return
    loading = true
    listError = ''
    try {
      catalog = await getBackend().rollbackVersions()
    } catch (e) {
      listError = message(e)
    } finally {
      loading = false
    }
  }

  /** @param {unknown} e */
  function message(e) {
    const code = rollbackErrorCode(e)
    const key = /** @type {Record<string, string>} */ (ROLLBACK_ERROR_KEYS)[code]
    return key ? t(key) : String(/** @type {any} */ (e)?.message ?? e)
  }

  /** @param {unknown} e */
  function fail(e) {
    errorCode = rollbackErrorCode(e)
    error = message(e)
    percent = null
    phase = 'confirm'
  }

  /** @param {string | null | undefined} iso */
  function formatDate(iso) {
    const date = iso ? new Date(iso) : null
    if (!date || Number.isNaN(date.getTime())) return ''
    try {
      return new Intl.DateTimeFormat(locale(), { dateStyle: 'medium' }).format(date)
    } catch {
      return iso?.slice(0, 10) ?? ''
    }
  }

  /** @param {import('../rollback.js').RollbackRow} row */
  function choose(row) {
    chosen = row
    error = ''
    errorCode = ''
    phase = 'confirm'
  }

  async function onInstall() {
    if (!chosen || !catalog || phase === 'installing') return
    phase = 'installing'
    error = ''
    errorCode = ''
    percent = null
    let update
    try {
      update = await asUpdate(await getBackend().rollbackPrepare({ version: chosen.version }))
    } catch (e) {
      fail(e)
      return
    }
    const before = rememberSkipped(catalog.currentVersion)
    try {
      await installUpdate(update, (progress) => {
        if (progress.percent !== null && progress.percent !== undefined) percent = progress.percent
      })
      phase = 'installed'
    } catch (e) {
      restoreSkipped(before)
      fail(e)
    }
  }

  async function onRestart() {
    try {
      await relaunchApp()
    } catch (e) {
      error = message(e)
    }
  }

  function close() {
    if (phase !== 'installing') open = false
  }
</script>

{#if open}
  <Modal
    title={phase === 'list' ? t('update.rollback.title') : t('update.rollback.confirmTitle', { version: target })}
    width={480}
    blocking={phase === 'installing'}
    onclose={close}
  >
    {#if phase === 'list'}
      <p class="intro">{t('update.rollback.intro')}</p>
      {#if catalog && !catalog.inPlace}
        <p class="note">{t('update.rollback.inPlaceNote', { bundle: catalog.bundle })}</p>
      {/if}
      {#if loading}
        <p class="muted">{t('update.rollback.loading')}</p>
      {:else if listError}
        <p class="error-msg">{listError}</p>
      {:else if catalog && !choosable}
        <p class="muted">{t('update.rollback.empty')}</p>
      {/if}
      {#if rows.length}
        <ul class="rows" data-testid="rollback-rows">
          {#each rows as row (row.version)}
            <li class="row" class:dim={row.state === 'belowFloor'} data-version={row.version} data-state={row.state}>
              <div class="row-text">
                <div class="row-head">
                  <span class="ver">{displayVersion(row.version)}</span>
                  {#if row.date}<span class="date">{formatDate(row.date)}</span>{/if}
                  {#if row.state === 'installed'}<span class="tag">{t('update.rollback.installed')}</span>{/if}
                </div>
                {#if row.notes}<p class="row-notes">{shortNotes(row.notes)}</p>{/if}
                {#if row.state === 'belowFloor'}<p class="why">{t('update.rollback.belowFloor')}</p>{/if}
              </div>
              {#if row.state === 'available'}
                <Button size="sm" variant="ghost" onclick={() => choose(row)}>{t('update.rollback.choose')}</Button>
              {:else if row.state === 'pageOnly'}
                <Button size="sm" variant="ghost" onclick={() => openExternal(row.pageUrl)}>{t('update.rollback.pageOnly')}</Button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    {:else if chosen}
      <div class="version-row">
        <span class="ver-curr">{current}</span>
        <span class="ver-arrow">→</span>
        <span class="ver-next">{target}</span>
        {#if chosen.date}<span class="date">{formatDate(chosen.date)}</span>{/if}
      </div>

      {#if chosen.notes}
        <div class="label">{t('update.rollback.notes')}</div>
        <div class="notes-box">{chosen.notes}</div>
      {/if}

      <div class="label">{t('update.rollback.warn.heading')}</div>
      <ul class="warnings" data-testid="rollback-warnings">
        {#if chosen.loses}
          <li class="strong">{t('update.rollback.warn.formatLoss', { version: target })}</li>
        {:else}
          <li>{t('update.rollback.warn.formatSame', { version: target })}</li>
        {/if}
        <li>{t('update.rollback.warn.settings', { version: target })}</li>
        <li>{t('update.rollback.warn.library')}</li>
        <li>{t('update.rollback.warn.features', { version: target })}</li>
        {#if chosen.honoursSkip}
          <li>{t('update.rollback.warn.skip', { current })}</li>
        {:else}
          <li>{t('update.rollback.warn.noSkip', { version: target, current })}</li>
        {/if}
        {#if jobsRunning > 0}
          <li class="strong">{t('update.rollback.warn.jobs', { count: jobsRunning })}</li>
        {/if}
      </ul>

      {#if phase === 'installing'}
        <div class="progress-section">
          <div class="progress-meta">
            <span>{t('update.status.downloading')}</span>
            <span class="progress-pct">{percent !== null ? `${percent}%` : ''}</span>
          </div>
          <div class="progress-track">
            <div
              class="progress-fill"
              class:indeterminate={percent === null}
              style={percent !== null ? `width: ${percent}%` : ''}
            ></div>
          </div>
        </div>
      {/if}

      {#if error}
        <div class="error-msg" role="alert">{error}</div>
      {/if}
    {/if}

    {#snippet buttons()}
      {#if phase === 'list'}
        {#if listError}
          <Button variant="ghost" onclick={load}>{t('update.rollback.action.retry')}</Button>
        {/if}
        <Button variant="ghost" onclick={close}>{t('update.rollback.action.close')}</Button>
      {:else if phase === 'installed'}
        <Button variant="ghost" onclick={close}>{t('update.action.later')}</Button>
        <Button variant="primary" onclick={onRestart}>{t('update.action.restart')}</Button>
      {:else}
        <Button variant="ghost" onclick={() => reset()} disabled={phase === 'installing'}>
          {t('update.rollback.action.back')}
        </Button>
        {#if offersPage(errorCode) && chosen?.pageUrl}
          <Button variant="ghost" onclick={() => chosen && openExternal(chosen.pageUrl)}>
            {t('update.rollback.action.page')}
          </Button>
        {/if}
        <Button variant="primary" onclick={onInstall} disabled={phase === 'installing'}>
          {#if phase === 'installing'}
            {percent !== null ? t('update.action.downloadingPercent', { percent }) : t('update.action.downloading')}
          {:else}
            {t('update.rollback.action.confirm', { version: target })}
          {/if}
        </Button>
      {/if}
    {/snippet}
  </Modal>
{/if}

<style>
  .intro,
  .note,
  .muted {
    margin: 0 0 var(--s-3);
    font-size: 12px;
    line-height: 1.5;
    color: var(--t2);
  }
  .note {
    color: var(--text);
  }
  .muted {
    color: var(--t3);
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 320px;
    overflow-y: auto;
    border: 1px solid var(--line2);
    border-radius: var(--r-chip);
    background: var(--panel);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-3) var(--s-4);
  }
  .row + .row {
    border-top: 1px solid var(--line2);
  }
  .row.dim .ver,
  .row.dim .row-notes {
    color: var(--t3);
  }
  .row-text {
    flex: 1;
    min-width: 0;
  }
  .row-head {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
    flex-wrap: wrap;
  }
  .ver {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
  .date {
    font-size: 11px;
    color: var(--t3);
  }
  .tag {
    font-size: 10.5px;
    color: var(--t2);
    border: 1px solid var(--line2);
    border-radius: 999px;
    padding: 0 var(--s-2);
  }
  .row-notes,
  .why {
    margin: 2px 0 0;
    font-size: 11.5px;
    line-height: 1.45;
    color: var(--t2);
    overflow-wrap: anywhere;
  }
  .why {
    color: var(--warn);
  }
  .version-row {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
    margin-bottom: var(--s-4);
    font-size: 12.5px;
  }
  .ver-curr {
    color: var(--t2);
  }
  .ver-arrow {
    color: var(--t3);
    font-size: 11px;
  }
  .ver-next {
    color: var(--text);
    font-weight: 600;
  }
  .label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--t3);
    font-weight: 600;
    margin-bottom: var(--s-2);
  }
  .notes-box {
    max-height: 120px;
    overflow-y: auto;
    background: var(--panel);
    border: 1px solid var(--line2);
    border-radius: var(--r-chip);
    padding: var(--s-3) var(--s-4);
    margin-bottom: var(--s-4);
    font-size: 12px;
    line-height: 1.55;
    color: var(--text);
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
    -webkit-user-select: text;
  }
  .warnings {
    margin: 0 0 var(--s-4);
    padding-left: 1.2em;
    font-size: 12px;
    line-height: 1.5;
    color: var(--t2);
  }
  .warnings li + li {
    margin-top: var(--s-1);
  }
  .warnings .strong {
    color: var(--text);
    font-weight: 600;
  }
  .progress-section {
    margin-bottom: var(--s-4);
  }
  .progress-meta {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 11.5px;
    color: var(--t2);
    margin-bottom: var(--s-2);
  }
  .progress-pct {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    color: var(--text);
  }
  .progress-track {
    height: 4px;
    background: var(--line2);
    border-radius: 2px;
    overflow: hidden;
    position: relative;
  }
  .progress-fill {
    height: 100%;
    background: var(--accent);
    border-radius: 2px;
    transition: width var(--dur-fast) var(--ease);
  }
  .progress-fill.indeterminate {
    width: 35%;
    position: absolute;
    animation: indeterminate 1.4s infinite ease-in-out;
  }
  .error-msg {
    margin-bottom: var(--s-4);
    font-size: 11.5px;
    color: var(--warn);
    line-height: 1.4;
  }
  @keyframes indeterminate {
    0% {
      left: -35%;
    }
    100% {
      left: 100%;
    }
  }
</style>
