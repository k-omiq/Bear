<script>
  /**
   * Settings > Data folder: the one folder Manga Cleaner and the typesetter
   * both keep their libraries in (`docs/shared-data-folder.md`).
   *
   * What it offers follows the mode. Legacy: set one up, which moves this
   * library in. Pending: the typesetter set one up, move this library into
   * it. Shared: where it is, move it, or point at another one. Unavailable is
   * the shell's blocking screen, so it only shows its reason here.
   *
   * A change is refused while a chapter is open or a job runs, and a change
   * of the whole folder while the typesetter is open: the backend refuses them
   * too, and says so, but a button that is bound to fail should say why first.
   * A move can take minutes; `dataFolder.busy` holds Settings open meanwhile.
   *
   * The Save choice is a plain setting, `saveCleanedTo`, read by the backend
   * when Save runs (`exporting.rs#save_to_typesetter`). It is read from and
   * written to the settings file directly: nothing else in the interface
   * needs it.
   *
   * @type {{ backend?: import('../api/backend.js').Backend }}
   */
  import { onMount, untrack } from 'svelte'
  import { Button, Field, Segmented, TextInput } from '../ui/index.js'
  import { getBackend } from '../api/backend.js'
  import { chooseFolder } from '../api/folder.js'
  import { app } from '../state/app.svelte.js'
  import { runningCount } from '../state/jobs.svelte.js'
  import { writeSettingsSerialized } from '../state/settingswrite.js'
  import {
    cancelDataFolderConfirm,
    changeDataFolder,
    confirmTypesetterClosed,
    dataFolder,
    deleteOldCopy,
    loadDataFolder,
  } from '../state/datafolder.svelte.js'
  import { t } from '../i18n/index.js'

  let { backend: givenBackend } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())

  const uid = $props.id()

  /** The folder a new data folder is made as, inside the one chosen. */
  const FOLDER_NAME = 'Manga Library'

  const status = $derived(dataFolder.status)
  const mode = $derived(status?.mode ?? 'legacy')

  /** Which change has its folder field open, in Shared mode. @type {'move'|'useExisting'|null} */
  let opened = $state(null)
  /** The folder field of the open change. */
  let target = $state('')
  /** The old copy waiting for its second press. @type {string|null} */
  let deleting = $state(null)
  let choosing = $state(false)

  /** @type {'data-folder'|'next-to-raws'} */
  let saveTo = $state('data-folder')
  let saveFailed = $state(false)

  onMount(() => {
    void loadDataFolder(backend)
    Promise.resolve()
      .then(() => backend.readSettings())
      .then((settings) => {
        if (settings?.saveCleanedTo === 'next-to-raws') saveTo = 'next-to-raws'
      })
      .catch(() => {})
  })

  // Legacy proposes the default place, so Set up is one press away.
  $effect(() => {
    if (mode === 'legacy' && !opened && !target && status?.defaultRoot) target = status.defaultRoot
  })

  /**
   * Why no change can start now, as a key, or null. A change in flight is
   * shown by its own line, not as a reason.
   */
  const blocked = $derived(
    app.route.name === 'editor'
      ? 'settings.dataFolder.blocked.chapterOpen'
      : runningCount() > 0
        ? 'settings.dataFolder.blocked.jobsRunning'
        : null
  )
  /** Moving or replacing the whole folder also needs the typesetter closed. */
  const typesetterOpen = $derived(status?.typesetterRunning === 'yes')
  const busy = $derived(dataFolder.busy !== null)

  const modeKey = $derived(
    {
      legacy: 'settings.dataFolder.mode.legacy',
      pending: 'settings.dataFolder.mode.pending',
      shared: 'settings.dataFolder.mode.shared',
      unavailable: 'settings.dataFolder.mode.unavailable',
    }[mode]
  )

  const saveOptions = $derived([
    { value: 'data-folder', label: t('settings.dataFolder.save.dataFolder') },
    { value: 'next-to-raws', label: t('settings.dataFolder.save.nextToRaws') },
  ])

  /**
   * `chosen` with the folder name appended, in the separator it already uses.
   *
   * @param {string} chosen
   */
  function inside(chosen) {
    const sep = chosen.includes('\\') && !chosen.includes('/') ? '\\' : '/'
    return `${chosen.replace(/[\\/]+$/, '')}${sep}${FOLDER_NAME}`
  }

  /**
   * Open the chooser. For a new folder it picks where to put one; for an
   * existing one it picks the folder itself.
   *
   * @param {'new'|'existing'} kind
   */
  async function browse(kind) {
    if (choosing) return
    choosing = true
    try {
      const chosen = await chooseFolder({
        title: t(kind === 'new' ? 'settings.dataFolder.chooserNew' : 'settings.dataFolder.chooserExisting'),
        defaultPath: status?.root ?? undefined,
      })
      if (chosen) target = kind === 'new' ? inside(chosen) : chosen
    } catch (error) {
      dataFolder.error = String(error)
    } finally {
      choosing = false
    }
  }

  /** @param {'move'|'useExisting'} kind */
  function open(kind) {
    opened = opened === kind ? null : kind
    target = ''
    dataFolder.error = null
  }

  /** @param {'setup'|'move'|'useExisting'} kind @param {string} where */
  async function change(kind, where) {
    const path = where.trim()
    if (!path) return
    const done = await changeDataFolder(kind, path, { backend })
    if (done) {
      opened = null
      target = ''
    }
  }

  /** @param {string} path */
  async function removeCopy(path) {
    deleting = null
    await deleteOldCopy(path, backend)
  }

  /** @param {string} value */
  async function changeSaveTo(value) {
    const previous = saveTo
    saveTo = value === 'next-to-raws' ? 'next-to-raws' : 'data-folder'
    saveFailed = false
    try {
      await writeSettingsSerialized(backend, { saveCleanedTo: saveTo })
    } catch {
      saveTo = previous
      saveFailed = true
    }
  }

  /** @param {string} iso */
  function day(iso) {
    const date = new Date(iso)
    return Number.isNaN(date.getTime()) ? iso : date.toLocaleDateString()
  }
</script>

<p class="intro">{t('settings.dataFolder.intro')}</p>

{#if status?.recovered && !dataFolder.recoveredSeen}
  <div class="note" role="status">
    <span>{t('settings.dataFolder.recovered', { detail: status.recovered })}</span>
    <Button size="sm" onclick={() => (dataFolder.recoveredSeen = true)}>{t('shell.action.dismissNotice')}</Button>
  </div>
{/if}

<h3 class="sub">{t('settings.dataFolder.where')}</h3>
<div class="where" data-mode={mode}>
  <p class="mode">{t(modeKey, { path: status?.root ?? '', reason: status?.reason ?? '' })}</p>
  {#if mode === 'shared' && status?.root}
    <p class="path" title={status.root}>{status.root}</p>
  {:else if status?.library}
    <p class="path quiet" title={status.library}>{t('settings.dataFolder.libraryAt', { path: status.library })}</p>
  {/if}
</div>

{#if status?.synced}
  <p class="warn" role="note">{t('settings.dataFolder.synced')}</p>
{/if}
<!-- Only the whole folder's moves need the typesetter closed; moving this
     library into the shared folder (Pending) does not. -->
{#if typesetterOpen && mode === 'shared'}
  <p class="warn" role="note">{t('settings.dataFolder.typesetterOpen')}</p>
{/if}
{#if blocked && !busy}
  <p class="warn" role="note">{t(blocked)}</p>
{/if}

{#if busy}
  <p class="busy" role="status" aria-live="polite">
    {t(dataFolder.busy === 'delete' ? 'settings.dataFolder.deleting' : 'settings.dataFolder.busy')}
  </p>
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

{#snippet folderField(kind, labelKey, descriptionKey)}
  <Field label={t(labelKey)} description={t(descriptionKey)} controlId="{uid}-target">
    {#snippet children()}
      <div class="row">
        <TextInput id="{uid}-target" value={target} onchange={(value) => (target = value)} disabled={busy} />
        <Button onclick={() => void browse(kind)} disabled={busy || choosing}>{t('shell.action.chooseFolder')}</Button>
      </div>
    {/snippet}
  </Field>
{/snippet}

{#if mode === 'legacy'}
  <div class="block">
    {@render folderField('new', 'settings.dataFolder.newLabel', 'settings.dataFolder.newDescription')}
    <div class="actions">
      <Button
        variant="primary"
        disabled={busy || !!blocked || !target.trim()}
        onclick={() => void change('setup', target)}
      >{t('settings.dataFolder.setup')}</Button>
    </div>
  </div>
{:else if mode === 'pending' && status?.root}
  <div class="block">
    <p class="quiet">{t('settings.dataFolder.pendingBody')}</p>
    <div class="actions">
      <Button
        variant="primary"
        disabled={busy || !!blocked}
        onclick={() => void change('setup', status.root ?? '')}
      >{t('settings.dataFolder.moveInto', { path: status.root })}</Button>
    </div>
  </div>
{:else if mode === 'shared'}
  <div class="block">
    <div class="actions">
      <Button
        disabled={busy || !!blocked || typesetterOpen}
        aria-expanded={opened === 'move'}
        onclick={() => open('move')}
      >{t('settings.dataFolder.move')}</Button>
      <Button
        disabled={busy || !!blocked || typesetterOpen}
        aria-expanded={opened === 'useExisting'}
        onclick={() => open('useExisting')}
      >{t('settings.dataFolder.useExisting')}</Button>
    </div>
    {#if opened === 'move'}
      {@render folderField('new', 'settings.dataFolder.moveLabel', 'settings.dataFolder.moveDescription')}
      <div class="actions">
        <Button
          variant="primary"
          disabled={busy || !!blocked || typesetterOpen || !target.trim()}
          onclick={() => void change('move', target)}
        >{t('settings.dataFolder.moveConfirm')}</Button>
        <Button disabled={busy} onclick={() => open('move')}>{t('shell.action.cancel')}</Button>
      </div>
    {:else if opened === 'useExisting'}
      {@render folderField('existing', 'settings.dataFolder.existingLabel', 'settings.dataFolder.existingDescription')}
      <div class="actions">
        <Button
          variant="primary"
          disabled={busy || !!blocked || typesetterOpen || !target.trim()}
          onclick={() => void change('useExisting', target)}
        >{t('settings.dataFolder.useConfirm')}</Button>
        <Button disabled={busy} onclick={() => open('useExisting')}>{t('shell.action.cancel')}</Button>
      </div>
    {/if}
  </div>
{/if}

{#if status?.oldCopies?.length}
  <h3 class="sub">{t('settings.dataFolder.oldCopies.title')}</h3>
  <p class="quiet">{t('settings.dataFolder.oldCopies.body')}</p>
  <ul class="copies">
    {#each status.oldCopies as copy (copy.path)}
      <li class="copy">
        <div class="facts">
          <span class="path" title={copy.path}>{copy.path}</span>
          <span class="quiet">{t('settings.dataFolder.oldCopies.size', { bytes: copy.bytes, date: day(copy.movedAt) })}</span>
        </div>
        {#if deleting === copy.path}
          <div class="confirm-inline" role="group" aria-label={t('settings.dataFolder.oldCopies.confirm')}>
            <span class="warn-text">{t('settings.dataFolder.oldCopies.confirm')}</span>
            <Button variant="primary" size="sm" disabled={busy} onclick={() => void removeCopy(copy.path)}>{t('settings.dataFolder.oldCopies.confirmAction')}</Button>
            <Button size="sm" onclick={() => (deleting = null)}>{t('shell.action.cancel')}</Button>
          </div>
        {:else}
          <Button size="sm" disabled={busy} onclick={() => (deleting = copy.path)}>{t('settings.dataFolder.oldCopies.delete')}</Button>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<div class="save">
  <Field label={t('settings.dataFolder.save.label')} description={t('settings.dataFolder.save.description')}>
    {#snippet children({ labelId, descriptionId })}
      <Segmented
        options={saveOptions}
        value={saveTo}
        labelledBy={labelId}
        describedBy={descriptionId}
        onchange={(value) => void changeSaveTo(value)}
      />
    {/snippet}
  </Field>
</div>
{#if saveFailed}
  <p class="error" role="alert">{t('settings.dataFolder.save.failed')}</p>
{/if}

<style>
  .intro,
  .quiet {
    margin: 0 0 var(--s-2);
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--t3);
  }

  .sub {
    margin: var(--s-6) 0 var(--s-3);
    font-size: 12px;
    font-weight: 600;
    color: var(--t2);
  }

  .where { margin-bottom: var(--s-3) }
  .save { margin-top: var(--s-7) }
  .mode {
    margin: 0 0 var(--s-1);
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--text);
  }

  .path {
    margin: 0;
    font-size: 11.5px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    color: var(--t2);
    overflow-wrap: anywhere;
  }

  .warn,
  .error,
  .busy {
    margin: 0 0 var(--s-3);
    font-size: 11.5px;
    line-height: 1.5;
  }
  .warn,
  .error,
  .warn-text { color: var(--warn) }
  .busy { color: var(--accent) }

  .note,
  .confirm {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    margin: 0 0 var(--s-4);
    padding: var(--s-4);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--panel);
    font-size: 11.5px;
    line-height: 1.5;
  }
  .note {
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
  }
  .confirm p { margin: 0 }

  .block {
    display: flex;
    flex-direction: column;
    gap: var(--s-4);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s-3);
  }

  .row { display: flex; align-items: center; gap: var(--s-6) }
  .row :global(> *:first-child) { flex: 1; min-width: 0 }

  .copies {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
  }
  .copy {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-4);
  }
  .facts {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .facts .quiet { margin: 0 }
  .confirm-inline {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--s-3);
    font-size: 11.5px;
  }
</style>
