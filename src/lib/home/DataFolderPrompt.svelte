<script>
  /**
   * Home's first-run card for Legacy mode: no app has set up the shared data
   * folder yet, and this app asks once whether to set it up at the default
   * place (`docs/shared-data-folder.md` section 7, "First run asks once").
   *
   * Move runs Setup at `status.defaultRoot`, with the same typesetter-closed
   * confirm and the same busy cover as the Pending card. Choose folder opens
   * Settings > Data folder, where another place can be typed or picked. Not
   * now records the answer in this app's own session store, so the card does
   * not come back; Settings > Data folder still offers Setup. A Move that
   * worked records it too. Nothing moves without a click.
   *
   * Drawn only once the status has answered: Pending has its own card
   * (`DataFolderCard.svelte`), and Shared and Unavailable need neither.
   *
   * @type {{ backend?: import('../api/backend.js').Backend, onmoved?: () => void }}
   */
  import { untrack } from 'svelte'
  import { Button } from '../ui/index.js'
  import { getBackend } from '../api/backend.js'
  import {
    cancelDataFolderConfirm,
    changeDataFolder,
    confirmTypesetterClosed,
    dataFolder,
  } from '../state/datafolder.svelte.js'
  import { markDataFolderPromptAnswered, session } from '../state/session.svelte.js'
  import { runningCount } from '../state/jobs.svelte.js'
  import { openSettings } from '../dialogs/settingslinks.js'
  import { t } from '../i18n/index.js'

  let { backend: givenBackend, onmoved } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())

  const target = $derived(dataFolder.status?.defaultRoot ?? '')
  const busy = $derived(dataFolder.busy !== null)
  const jobsRunning = $derived(runningCount() > 0)
  const shown = $derived(dataFolder.status?.mode === 'legacy' && target !== '' && !session.dataFolderPromptAnswered)

  /** @param {boolean} done */
  function settled(done) {
    if (!done) return
    markDataFolderPromptAnswered()
    onmoved?.()
  }

  async function move() {
    if (busy || jobsRunning || !target) return
    settled(await changeDataFolder('setup', target, { backend }))
  }

  async function confirmed() {
    settled(await confirmTypesetterClosed(backend))
  }
</script>

{#if shown}
  <section class="card" aria-labelledby="data-folder-prompt-title">
    <h2 id="data-folder-prompt-title">{t('home.dataFolderPrompt.title')}</h2>
    <p>{t('home.dataFolderPrompt.body', { path: target })}</p>
    {#if jobsRunning && !busy}
      <p class="note">{t('settings.dataFolder.blocked.jobsRunning')}</p>
    {/if}
    {#if busy}
      <p class="note" role="status">{t('settings.dataFolder.busy')}</p>
    {/if}
    {#if dataFolder.confirm}
      <div role="alert">
        <p class="note">{t('settings.dataFolder.confirmClosed.body')}</p>
        <div class="actions">
          <Button variant="primary" size="sm" onclick={() => void confirmed()}>{t('settings.dataFolder.confirmClosed.action')}</Button>
          <Button size="sm" onclick={cancelDataFolderConfirm}>{t('shell.action.cancel')}</Button>
        </div>
      </div>
    {:else}
      <div class="actions">
        <Button variant="primary" size="sm" disabled={busy || jobsRunning} onclick={() => void move()}>{t('home.dataFolderPrompt.move')}</Button>
        <Button size="sm" disabled={busy} onclick={() => openSettings('dataFolder')}>{t('home.dataFolderPrompt.choose')}</Button>
        <Button size="sm" disabled={busy} onclick={markDataFolderPromptAnswered}>{t('home.dataFolderPrompt.notNow')}</Button>
      </div>
    {/if}
    {#if dataFolder.error}
      <p class="error" role="alert">{t('settings.dataFolder.failed', { detail: dataFolder.error })}</p>
    {/if}
  </section>
{/if}

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    max-width: 560px;
    margin: var(--s-6) 0 0;
    padding: var(--s-5) var(--s-6);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--panel);
  }

  h2 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
  }

  p {
    margin: 0;
    font-size: 12px;
    line-height: 1.5;
    color: var(--t2);
    overflow-wrap: anywhere;
  }

  .note { font-size: 11.5px; color: var(--t3) }
  .error { font-size: 11.5px; color: var(--warn) }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-3);
  }
</style>
