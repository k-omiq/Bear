<script>
  /**
   * "One folder for both apps": the centered dialog of start rows D (Join)
   * and F (Set up), `docs/shared-data-folder.md` section 5.4. It replaces
   * version 1's Home card.
   *
   * Top to bottom: a still drawing (the two apps, two arrows, one folder),
   * the title, two lines, the path row (Set up can change it, Join cannot:
   * the folder is the one Pear already uses), one line on what will happen,
   * and the two buttons. The same dialog then shows the move: Blocked while
   * Pear runs in the folder ("Close Pear first."), Working with the copy's
   * progress (it cannot be closed, and the window behind it is held), Done,
   * and Failed with the reason in plain words.
   *
   * Not on the modal stack (`App.svelte` mounts it): it belongs to the start,
   * as the first-launch screen does, and must stay up over the busy cover.
   * Every state lives in `state/datafolder.svelte.js#startDialog`.
   *
   * @type {{ backend?: import('../api/backend.js').Backend }}
   */
  import { untrack } from 'svelte'
  import { isDialogOutsideStackOpen, setDialogOutsideStack } from '../shortcuts.js'
  import { Button, Modal } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'
  import { getBackend } from '../api/backend.js'
  import { chooseFolder } from '../api/folder.js'
  import { app } from '../state/app.svelte.js'
  import { runningCount } from '../state/jobs.svelte.js'
  import {
    acceptStart,
    cancelDataFolderConfirm,
    checkStartAgain,
    closeStartDialog,
    dataFolder,
    declineStart,
    setStartTarget,
    startBlocked,
    startDialog,
  } from '../state/datafolder.svelte.js'
  import { openSendToPear } from '../state/sendtopear.svelte.js'
  import { copyMinutes, shownPath } from '../state/startflow.js'
  import { t } from '../i18n/index.js'

  let { backend: givenBackend } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())

  // Not on the stack, so the shortcut table is told a dialog is up.
  $effect(() => {
    const before = isDialogOutsideStackOpen()
    setDialogOutsideStack(true)
    return () => setDialogOutsideStack(before)
  })

  /** The folder a new data folder is made as, inside the one chosen. */
  const FOLDER_NAME = 'Manga Library'

  const home = $derived(dataFolder.status?.home ?? null)
  const plan = $derived(startDialog.plan)
  const phase = $derived(startDialog.phase)
  const join = $derived(startDialog.variant === 'join')
  const blocked = $derived(startBlocked())
  const jobsRunning = $derived(runningCount() > 0)
  const chapterOpen = $derived(app.route.name === 'editor')
  const titles = $derived(dataFolder.status?.titles ?? 0)
  let choosing = $state(false)

  /** What will happen, as one line. */
  const whatHappens = $derived.by(() => {
    if (!plan) return startDialog.planning ? t('startFolder.planning') : ''
    if (plan.nothingToMove) return t('startFolder.nothingToMove')
    const minutes = copyMinutes(plan.bytes)
    if (join) {
      return plan.sameVolume
        ? t('startFolder.joinSameVolume', { bytes: plan.bytes })
        : t('startFolder.joinOtherVolume', { bytes: plan.bytes, minutes })
    }
    return plan.sameVolume ? t('startFolder.setupSameVolume') : t('startFolder.setupOtherVolume', { bytes: plan.bytes, minutes })
  })

  const canGo = $derived(
    phase === 'ask' && !blocked && !jobsRunning && !chapterOpen && !startDialog.planning && !plan?.refusal && startDialog.target !== ''
  )

  /** `chosen` with the folder name appended, in the separator it already uses. @param {string} chosen */
  function inside(chosen) {
    const sep = chosen.includes('\\') && !chosen.includes('/') ? '\\' : '/'
    return `${chosen.replace(/[\\/]+$/, '')}${sep}${FOLDER_NAME}`
  }

  async function change() {
    if (choosing || join) return
    choosing = true
    try {
      const chosen = await chooseFolder({ title: t('startFolder.chooser'), defaultPath: home ?? undefined })
      if (chosen) await setStartTarget(inside(chosen), backend)
    } catch (error) {
      startDialog.error = String(error)
    } finally {
      choosing = false
    }
  }

  function chooseTitles() {
    closeStartDialog()
    openSendToPear()
  }
</script>

<Modal
  title={t('startFolder.title')}
  width={460}
  blocking
  describedby="start-folder-lines"
  onclose={phase === 'working' ? undefined : phase === 'ask' ? declineStart : closeStartDialog}
>
  <div class="start" data-phase={phase}>
    <svg class="drawing" viewBox="0 0 240 92" role="img" aria-label={t('startFolder.drawing')}>
      <rect class="app" x="22" y="6" width="44" height="44" rx="11" />
      <text class="mark" x="44" y="34" text-anchor="middle">MC</text>
      <rect class="app pear" x="174" y="6" width="44" height="44" rx="11" />
      <text class="mark" x="196" y="34" text-anchor="middle">P</text>
      <path class="arrow" d="M66 40 C 88 52, 96 58, 104 64" />
      <path class="arrow" d="M98 61 L104 64 L101 57" />
      <path class="arrow" d="M174 40 C 152 52, 144 58, 136 64" />
      <path class="arrow" d="M142 61 L136 64 L139 57" />
      <path class="folder" d="M100 62 h14 l5 5 h21 a4 4 0 0 1 4 4 v14 a4 4 0 0 1 -4 4 h-36 a4 4 0 0 1 -4 -4 v-19 a4 4 0 0 1 4 -4 z" />
    </svg>

    <div id="start-folder-lines" class="lines">
      <p>{t('startFolder.lineOne')}</p>
      <p>{t('startFolder.lineTwo')}</p>
    </div>

    <div class="path-row">
      <Icon name="folder" size={14} />
      <span class="path" title={shownPath(startDialog.target)}>{shownPath(startDialog.target, home)}</span>
      {#if !join && phase === 'ask'}
        <Button variant="plain" size="sm" disabled={choosing} onclick={() => void change()}>{t('startFolder.change')}</Button>
      {/if}
    </div>

    {#if phase === 'ask'}
      {#if whatHappens}<p class="what" role="status">{whatHappens}</p>{/if}
      {#if plan?.refusal}<p class="warn" role="alert">{plan.refusal}</p>{/if}
      {#if plan?.synced}<p class="warn" role="note">{t('settings.dataFolder.synced')}</p>{/if}
      {#if blocked}
        <div class="blocked" role="alert">
          <p>{t('startFolder.blocked')}</p>
          <Button size="sm" onclick={() => void checkStartAgain(backend)}>{t('startFolder.checkAgain')}</Button>
        </div>
      {:else if chapterOpen}
        <p class="warn" role="note">{t('settings.dataFolder.blocked.chapterOpen')}</p>
      {:else if jobsRunning}
        <p class="warn" role="note">{t('settings.dataFolder.blocked.jobsRunning')}</p>
      {/if}
      {#if dataFolder.confirm}
        <div class="blocked" role="alert">
          <p>{t('settings.dataFolder.confirmClosed.body')}</p>
          <div class="actions">
            <Button size="sm" variant="primary" onclick={() => void acceptStart({ confirmedClosed: true, backend })}>{t('settings.dataFolder.confirmClosed.action')}</Button>
            <Button size="sm" onclick={cancelDataFolderConfirm}>{t('shell.action.cancel')}</Button>
          </div>
        </div>
      {/if}
      {#if startDialog.error}<p class="warn" role="alert">{startDialog.error}</p>{/if}
    {:else if phase === 'working'}
      {@const progress = startDialog.progress}
      <div class="working" role="status" aria-live="polite">
        <div
          class="bar"
          role="progressbar"
          aria-label={t('startFolder.moving')}
          aria-valuemin={0}
          aria-valuemax={progress.total || 1}
          aria-valuenow={progress.total ? progress.copied : 0}
        >
          <span style:width={progress.total ? `${Math.min(100, (progress.copied / progress.total) * 100)}%` : '0%'} class:indeterminate={!progress.total}></span>
        </div>
        <p>{progress.total ? t('startFolder.copying', { copied: progress.copied, total: progress.total }) : t('startFolder.moving')}</p>
      </div>
    {:else if phase === 'done'}
      <p class="done" role="status">{t('startFolder.done', { path: shownPath(dataFolder.status?.root, home) })}</p>
      {#if titles > 0}<p>{t('startFolder.titles', { count: titles })}</p>{/if}
    {:else}
      <div role="alert">
        <p class="failed">{t('startFolder.failed')}</p>
        {#if startDialog.error}<p>{startDialog.error}</p>{/if}
        {#if startDialog.nothingChanged}<p>{t('startFolder.nothingChanged')}</p>{/if}
      </div>
    {/if}
  </div>

  {#snippet buttons()}
    {#if phase === 'ask'}
      <Button onclick={declineStart}>{t('startFolder.notNow')}</Button>
      <Button variant="primary" disabled={!canGo} onclick={() => void acceptStart({ backend })}>{t('startFolder.use')}</Button>
    {:else if phase === 'done' && titles > 0}
      <Button onclick={closeStartDialog}>{t('startFolder.later')}</Button>
      <Button variant="primary" onclick={chooseTitles}>{t('startFolder.chooseTitles')}</Button>
    {:else if phase === 'done'}
      <Button variant="primary" onclick={closeStartDialog}>{t('shell.action.done')}</Button>
    {:else if phase === 'failed'}
      <Button variant="primary" onclick={closeStartDialog}>{t('shell.action.close')}</Button>
    {/if}
  {/snippet}
</Modal>

<style>
  .start {
    display: flex;
    flex-direction: column;
    gap: var(--s-4);
  }
  .start p { margin: 0; line-height: 1.5; overflow-wrap: anywhere }

  .drawing {
    display: block;
    width: 200px;
    height: auto;
    margin: 0 auto var(--s-2);
  }
  .drawing .app { fill: var(--panel); stroke: var(--line2) }
  .drawing .pear { stroke: var(--accent) }
  .drawing .mark { fill: var(--t2); font-size: 13px; font-weight: 600 }
  .drawing .arrow { fill: none; stroke: var(--t3); stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round }
  .drawing .folder { fill: var(--accent); opacity: .85 }

  .lines {
    display: flex;
    flex-direction: column;
    gap: var(--s-1);
    font-size: 12.5px;
    color: var(--t2);
  }

  .path-row {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-3) var(--s-4);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--panel);
    color: var(--t2);
  }
  .path {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    color: var(--text);
    overflow-wrap: anywhere;
  }

  .what, .done { font-size: 12.5px; color: var(--text) }
  .warn, .failed { font-size: 12px; color: var(--warn) }

  .blocked {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--s-3);
    font-size: 12px;
  }
  .actions { display: flex; gap: var(--s-3) }

  .working {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    font-size: 12px;
    color: var(--t2);
  }
  .bar {
    height: 6px;
    overflow: hidden;
    border-radius: var(--r-pill);
    background: var(--line2);
  }
  .bar span {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: var(--accent);
    transition: width var(--dur) var(--ease);
  }
  .bar span.indeterminate { width: 30% !important; opacity: .6 }
</style>
