<script>
  /**
   * The data folder is set up but cannot be used: its drive is unplugged, its
   * marker is gone, it is another library's folder, or a newer version set it
   * up (`docs/shared-data-folder.md` section 5). There is no library to show,
   * so this takes the whole window in place of Home and the editor, which
   * would only fail on every call.
   *
   * Two ways out: try again once the drive is back, or point at another data
   * folder. The path field is the chooser's value and can be typed into, as
   * every folder field in the app can: outside the desktop window there is no
   * chooser to open.
   *
   * Everything behind it is inert while it is up (`App.svelte`), and focus
   * starts on Try again.
   *
   * @type {{ backend?: import('../api/backend.js').Backend }}
   */
  import { onMount, untrack } from 'svelte'
  import { Button } from '../ui/index.js'
  import { getBackend } from '../api/backend.js'
  import SourceFolderField from '../home/dialogs/SourceFolderField.svelte'
  import {
    cancelDataFolderConfirm,
    changeDataFolder,
    confirmTypesetterClosed,
    dataFolder,
    loadDataFolder,
  } from '../state/datafolder.svelte.js'
  import { t } from '../i18n/index.js'

  let { backend: givenBackend } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())

  let checking = $state(false)
  let choosing = $state(false)
  let target = $state('')
  const busy = $derived(checking || dataFolder.busy !== null)

  /** @type {HTMLElement|undefined} */
  let root = $state()

  // The screen is the window now: focus starts on its first way out.
  onMount(() => {
    root?.querySelector('button')?.focus()
  })

  async function retry() {
    if (busy) return
    checking = true
    dataFolder.error = null
    try {
      await loadDataFolder(backend)
    } finally {
      checking = false
    }
  }

  async function useOther() {
    const path = target.trim()
    if (!path || busy) return
    if (await changeDataFolder('useExisting', path, { backend })) target = ''
  }
</script>

<div bind:this={root} class="blocked" role="alertdialog" aria-modal="true" aria-labelledby="data-folder-title" aria-describedby="data-folder-reason">
  <div class="card">
    <h1 id="data-folder-title">{t('shell.dataFolder.title')}</h1>
    <p id="data-folder-reason" class="reason">{dataFolder.status?.reason || t('shell.dataFolder.reasonUnknown')}</p>
    <p class="quiet">{t('shell.dataFolder.body')}</p>

    <div class="actions">
      <Button variant="primary" disabled={busy} onclick={() => void retry()}>{t('shell.dataFolder.retry')}</Button>
    </div>

    {#if choosing}
      <div class="other">
        <SourceFolderField
          value={target}
          onchange={(value) => (target = value)}
          label={t('shell.dataFolder.pathLabel')}
          description={t('shell.dataFolder.pathDescription')}
          browseLabel={t('shell.action.chooseFolder')}
          chooserTitle={t('settings.dataFolder.chooserExisting')}
          defaultPath={dataFolder.status?.defaultRoot ?? undefined}
          disabled={busy}
        />
        <div class="actions">
          <Button variant="primary" disabled={busy || !target.trim()} onclick={() => void useOther()}>{t('settings.dataFolder.useConfirm')}</Button>
          <Button disabled={busy} onclick={() => (choosing = false)}>{t('shell.action.cancel')}</Button>
        </div>
      </div>
    {:else}
      <div class="actions">
        <Button disabled={busy} onclick={() => (choosing = true)}>{t('shell.dataFolder.choose')}</Button>
      </div>
    {/if}

    {#if dataFolder.busy}
      <p class="quiet" role="status">{t('settings.dataFolder.busy')}</p>
    {/if}

    {#if dataFolder.confirm}
      <div class="confirm" role="alert">
        <p>{t('settings.dataFolder.confirmClosed.body')}</p>
        <div class="actions">
          <Button variant="primary" onclick={() => void confirmTypesetterClosed(backend)}>{t('settings.dataFolder.confirmClosed.action')}</Button>
          <Button onclick={cancelDataFolderConfirm}>{t('shell.action.cancel')}</Button>
        </div>
      </div>
    {/if}

    {#if dataFolder.error}
      <p class="error" role="alert">{t('settings.dataFolder.failed', { detail: dataFolder.error })}</p>
    {/if}
  </div>
</div>

<style>
  .blocked {
    position: fixed;
    inset: 0;
    z-index: 70;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--s-8);
    background: var(--bg);
    color: var(--text);
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: var(--s-4);
    width: 100%;
    max-width: 480px;
  }

  h1 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
  }

  .reason {
    margin: 0;
    font-size: 13px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .quiet,
  .error {
    margin: 0;
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--t3);
  }
  .error { color: var(--warn) }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-3);
  }

  .other {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
  }

  .confirm {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    padding: var(--s-4);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--panel);
    font-size: 11.5px;
  }
  .confirm p { margin: 0 }
</style>
