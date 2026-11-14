<script>
  import { Modal, Button } from '../ui/index.js'
  import { installUpdate, relaunchApp } from '../updater.js'
  import { t } from '../i18n/index.js'
  import { getBackend } from '../api/backend.js'
  import { clearSkipped } from '../rollback.js'
  import RollbackDialog from './RollbackDialog.svelte'

  /**
   * @type {{
   *   open: boolean,
   *   update: import('@tauri-apps/plugin-updater').Update | null,
   * }}
   */
  let { open = $bindable(false), update = null } = $props()

  let currentVersion = $state('')
  let downloading = $state(false)
  let installed = $state(false)
  let percent = $state(/** @type {number | null} */ (null))
  let error = $state('')
  let rollbackOpen = $state(false)

  $effect(() => {
    if (open) {
      error = ''
      if (!currentVersion) {
        if (update?.currentVersion) {
          currentVersion = update.currentVersion
        } else {
          getBackend()
            .about()
            .then((info) => {
              currentVersion = info?.appVersion ?? ''
            })
            .catch(() => {
              currentVersion = ''
            })
        }
      }
    }
  })

  const curVer = $derived(
    currentVersion ? (currentVersion.startsWith('v') ? currentVersion : `v${currentVersion}`) : '·'
  )
  const newVer = $derived(
    update?.version ? (update.version.startsWith('v') ? update.version : `v${update.version}`) : '·'
  )

  async function onDownload() {
    if (!update || downloading || installed) return
    downloading = true
    installed = false
    percent = null
    error = ''
    try {
      await installUpdate(update, (progress) => {
        if (progress.percent !== null && progress.percent !== undefined) {
          percent = progress.percent
        }
      })
      downloading = false
      installed = true
      // Updating on purpose ends a skip left by an earlier rollback.
      clearSkipped()
    } catch (e) {
      downloading = false
      installed = false
      percent = null
      const msg = e?.message ?? String(e)
      error = msg
    }
  }

  async function onRestart() {
    try {
      await relaunchApp()
    } catch (e) {
      error = e?.message ?? String(e)
    }
  }
</script>

{#if open}
  <Modal
    title={t('update.title')}
    width={420}
    blocking={downloading}
    onclose={() => {
      if (!downloading) open = false
    }}
  >
    <div class="version-row">
      <span class="version-label">{t('update.field.version')}</span>
      <div class="version-val">
        <span class="ver-curr">{curVer}</span>
        <span class="ver-arrow">→</span>
        <span class="ver-next">{newVer}</span>
      </div>
    </div>

    <div class="notes-section">
      <div class="notes-label">{t('update.field.releaseNotes')}</div>
      {#if update?.body && update.body.trim()}
        <div class="notes-box">{update.body.trim()}</div>
      {:else}
        <div class="notes-box empty">{t('update.notes.empty')}</div>
      {/if}
    </div>

    {#if downloading}
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
      <div class="error-msg">{error}</div>
    {/if}

    {#if !downloading && !installed}
      <button
        type="button"
        class="rollback-link"
        onclick={() => {
          open = false
          rollbackOpen = true
        }}
      >
        {t('update.rollback.openFromUpdate')}
      </button>
    {/if}

    {#snippet buttons()}
      {#if installed}
        <Button
          variant="ghost"
          onclick={() => (open = false)}
        >
          {t('update.action.later')}
        </Button>
        <Button
          variant="primary"
          onclick={onRestart}
        >
          {t('update.action.restart')}
        </Button>
      {:else}
        <Button
          variant="ghost"
          onclick={() => (open = false)}
          disabled={downloading}
        >
          {t('update.action.later')}
        </Button>
        <Button
          variant="primary"
          onclick={onDownload}
          disabled={downloading || !update}
        >
          {#if downloading}
            {percent !== null ? t('update.action.downloadingPercent', { percent }) : t('update.action.downloading')}
          {:else}
            {t('update.action.download')}
          {/if}
        </Button>
      {/if}
    {/snippet}
  </Modal>
{/if}

<RollbackDialog bind:open={rollbackOpen} />

<style>
  .version-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--s-4);
    font-size: 12px;
  }
  .version-label {
    color: var(--t3);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-weight: 600;
  }
  .version-val {
    display: flex;
    align-items: center;
    gap: var(--s-2);
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
  .notes-section {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    margin-bottom: var(--s-4);
  }
  .notes-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--t3);
    font-weight: 600;
  }
  .notes-box {
    max-height: 180px;
    min-height: 60px;
    overflow-y: auto;
    background: var(--panel);
    border: 1px solid var(--line2);
    border-radius: var(--r-chip);
    padding: var(--s-3) var(--s-4);
    font-size: 12px;
    line-height: 1.55;
    color: var(--text);
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
    -webkit-user-select: text;
  }
  .notes-box.empty {
    display: flex;
    align-items: center;
    color: var(--t3);
    font-style: italic;
    white-space: normal;
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
  .rollback-link {
    display: block;
    margin: 0 0 var(--s-2);
    padding: 0;
    border: 0;
    background: none;
    font: inherit;
    font-size: 11.5px;
    color: var(--t2);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .rollback-link:hover {
    color: var(--text);
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
