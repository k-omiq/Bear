<script>
  /**
   * Home's card for Pending mode: the typesetter set up a shared data folder,
   * and this library is still in its old place. Move puts it there now; Later
   * hides the card for this session, and Settings > Data folder still offers
   * the move.
   *
   * While the move runs the shell holds the window under its busy cover, so
   * nothing opens a chapter or starts a run meanwhile; Home lists the library
   * again when it ends (`dataFolder.generation`).
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
  import { runningCount } from '../state/jobs.svelte.js'
  import { t } from '../i18n/index.js'

  let { backend: givenBackend, onmoved } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())

  const root = $derived(dataFolder.status?.root ?? '')
  const busy = $derived(dataFolder.busy !== null)
  const jobsRunning = $derived(runningCount() > 0)

  async function move() {
    if (busy || !root) return
    if (await changeDataFolder('setup', root, { backend })) onmoved?.()
  }

  async function confirmed() {
    if (await confirmTypesetterClosed(backend)) onmoved?.()
  }
</script>

{#if dataFolder.status?.mode === 'pending' && root && !dataFolder.pendingLater}
  <section class="card" aria-labelledby="data-folder-card-title">
    <h2 id="data-folder-card-title">{t('home.dataFolder.title')}</h2>
    <p>{t('home.dataFolder.body', { path: root })}</p>
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
        <Button variant="primary" size="sm" disabled={busy || jobsRunning} onclick={() => void move()}>{t('home.dataFolder.move')}</Button>
        <Button size="sm" disabled={busy} onclick={() => (dataFolder.pendingLater = true)}>{t('home.dataFolder.later')}</Button>
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
    gap: var(--s-3);
  }
</style>
