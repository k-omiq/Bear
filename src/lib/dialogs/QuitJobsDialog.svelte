<script>
  /**
   * The quit guard's question (`app://quit-requested`): the window was closed,
   * or Quit was chosen, while jobs are running.
   *
   * Three answers, given to `state/jobs.svelte.js#answerQuit`:
   *
   * - **Stop jobs and quit** (`quit`): every job is stopped, so what finished
   *   is kept and a stopped clean can be resumed, then the app quits.
   * - **Keep running in background** (`hide`): only when there is a tray to
   *   come back from. The window hides and the jobs go on.
   * - **Cancel**, Escape or the backdrop: nothing happens, the app stays.
   *
   * Cancel is first in the row and so takes the focus: a stray Return must
   * not end the user's jobs.
   *
   * ## Beside the stack, not on it
   *
   * `App.svelte` draws this over whatever dialog is up rather than pushing it,
   * because the modal host mounts only the top of the stack and a push would
   * tear down the dialog underneath (`askQuit` says what that would lose).
   * Two mounted `Modal`s both listen for keys on `window`, so this one takes
   * Escape and Tab first, in the capture phase, and keeps them from the one
   * below: Escape answers Cancel here and leaves that dialog open, and Tab
   * cycles through these buttons only. The shortcut layer is told a dialog it
   * cannot see is up (`setDialogOutsideStack`), as the first-launch offer does.
   */
  import { Button, Modal } from '../ui/index.js'
  import { cycleTab } from '../ui/focus.js'
  import { answerQuit, jobs, runningJobs } from '../state/jobs.svelte.js'
  import { isDialogOutsideStackOpen, setDialogOutsideStack } from '../shortcuts.js'
  import { t } from '../i18n/index.js'
  import { jobTitle } from '../shell/jobtitle.js'

  const canHide = $derived(jobs.quit?.canHide === true)
  const listed = $derived(runningJobs())
  const count = $derived(Math.max(listed.length, Number(jobs.quit?.count) || 0))

  /** How many names the list shows before it stops; the count says the rest. */
  const SHOWN = 5

  /** @type {HTMLElement|undefined} */
  let root = $state()

  $effect(() => {
    const before = isDialogOutsideStackOpen()
    setDialogOutsideStack(true)
    /** @param {KeyboardEvent} event */
    const onkey = (event) => {
      if (event.key === 'Escape') {
        event.preventDefault()
        event.stopPropagation()
        answerQuit(null)
      } else if (event.key === 'Tab') {
        event.stopPropagation()
        const dialog = root?.closest('[role="dialog"]')
        if (dialog instanceof HTMLElement) cycleTab(event, dialog)
      }
    }
    window.addEventListener('keydown', onkey, true)
    return () => {
      window.removeEventListener('keydown', onkey, true)
      setDialogOutsideStack(before)
    }
  })
</script>

<Modal title={t('jobs.quit.title')} width={420} onclose={() => answerQuit(null)}>
  <div class="quit" data-count={count} bind:this={root}>
    <p class="body">{t('jobs.quit.body', { count })}</p>
    {#if listed.length}
      <ul class="names">
        {#each listed.slice(0, SHOWN) as job (job.runId)}
          <li>{jobTitle(job)}</li>
        {/each}
      </ul>
    {/if}
    {#if canHide}<p class="note">{t('jobs.quit.hideNote')}</p>{/if}
  </div>

  {#snippet buttons()}
    <Button onclick={() => answerQuit('cancel')}>{t('shell.action.cancel')}</Button>
    {#if canHide}
      <Button onclick={() => answerQuit('hide')}>{t('jobs.quit.keep')}</Button>
    {/if}
    <Button variant="primary" onclick={() => answerQuit('quit')}>{t('jobs.quit.stop')}</Button>
  {/snippet}
</Modal>

<style>
  .quit { display: grid; gap: var(--s-4); margin-top: var(--s-3) }
  .body { margin: 0; font-size: 12.5px; line-height: 1.5; color: var(--text) }
  .names {
    display: grid;
    gap: 3px;
    margin: 0;
    padding: 0 0 0 var(--s-5);
    font-size: 12px;
    line-height: 1.45;
    color: var(--t2);
  }
  .names li { overflow: hidden; text-overflow: ellipsis; white-space: nowrap }
  .note { margin: 0; font-size: 11px; line-height: 1.45; color: var(--t3) }
</style>
