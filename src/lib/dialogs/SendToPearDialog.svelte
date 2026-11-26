<script>
  /**
   * Send to Pear's title list (`docs/shared-data-folder.md` section 8.4.1).
   * Reached from the data folder dialog's Done state, from Home while the
   * folder is shared, and from Settings > Data folder.
   *
   * One row per title: a checkbox, the name, "12 chapters, 9 cleaned".
   * Checked by default when a chapter has a cleaned page; a title with none
   * cannot be checked and says "Nothing cleaned yet". A title with a chapter
   * sent before says so, and sends only what changed unless "Send all again"
   * is on. One button sends the checked titles in the background and closes
   * the list; the shell then shows "Sending 40 of 63 chapters" with Stop
   * (`shell/SendToPearStatus.svelte`).
   *
   * @type {{ spec: import('../state/app.svelte.js').ModalSpec, backend?: import('../api/backend.js').Backend }}
   */
  import { onMount, untrack } from 'svelte'
  import { Button, Modal } from '../ui/index.js'
  import { getBackend } from '../api/backend.js'
  import { closeModal } from '../state/app.svelte.js'
  import {
    checkTitle,
    chosenChapters,
    chosenTitles,
    loadSendTitles,
    sendRunning,
    sendToPear,
    startSend,
    stopSend,
    titleSendable,
  } from '../state/sendtopear.svelte.js'
  import { t } from '../i18n/index.js'

  let { backend: givenBackend } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())
  const uid = $props.id()

  onMount(() => {
    void loadSendTitles(backend)
  })

  const titles = $derived(sendToPear.titles ?? [])
  const count = $derived(chosenTitles().length)
  const chapters = $derived(chosenChapters())
  const running = $derived(sendRunning())
  let starting = $state(false)

  async function send() {
    if (starting || running || count === 0 || chapters === 0) return
    starting = true
    try {
      if (await startSend(backend)) closeModal('send')
    } finally {
      starting = false
    }
  }
</script>

<Modal title={t('sendToPear.title')} width={480} onclose={() => closeModal(null)}>
  <div class="send">
    <p class="intro">{t('sendToPear.intro')}</p>

    {#if running && sendToPear.progress}
      <div class="running" role="status">
        <span>{t('sendToPear.sending', { done: sendToPear.progress.done, total: sendToPear.progress.total })}</span>
        <Button size="sm" onclick={() => void stopSend(backend)}>{t('sendToPear.stop')}</Button>
      </div>
    {/if}

    {#if sendToPear.loading && !sendToPear.titles}
      <p class="quiet" role="status">{t('sendToPear.loading')}</p>
    {:else if titles.length === 0 && !sendToPear.error}
      <p class="quiet">{t('sendToPear.empty')}</p>
    {:else}
      <ul class="titles">
        {#each titles as title (title.projectId)}
          {@const sendable = titleSendable(title)}
          <li class="title" class:off={!sendable}>
            <input
              id="{uid}-{title.projectId}"
              type="checkbox"
              checked={sendable && sendToPear.checked[title.projectId] === true}
              disabled={!sendable || running}
              onchange={(event) => checkTitle(title.projectId, /** @type {HTMLInputElement} */ (event.currentTarget).checked)}
            />
            <label for="{uid}-{title.projectId}">
              <span class="name">{title.name}</span>
              <span class="facts">
                {#if sendable}
                  {t('sendToPear.counts', { chapters: title.chapters, cleaned: title.cleaned })}
                {:else}
                  {t('sendToPear.nothingCleaned')}
                {/if}
                {#if title.sent > 0}<span class="tag">{t('sendToPear.sentBefore')}</span>{/if}
              </span>
            </label>
          </li>
        {/each}
      </ul>

      <div class="again">
        <input id="{uid}-again" type="checkbox" bind:checked={sendToPear.again} disabled={running} />
        <label for="{uid}-again">
          <span class="name">{t('sendToPear.again')}</span>
          <span class="facts">{t('sendToPear.againDescription')}</span>
        </label>
      </div>

      {#if count > 0 && chapters === 0}
        <p class="quiet" role="note">{t('sendToPear.nothingNew')}</p>
      {/if}
    {/if}

    {#if sendToPear.error}
      <p class="error" role="alert">{t('sendToPear.failed', { detail: sendToPear.error })}</p>
    {/if}
  </div>

  {#snippet buttons()}
    <Button onclick={() => closeModal(null)}>{t('shell.action.cancel')}</Button>
    <Button variant="primary" disabled={running || starting || count === 0 || chapters === 0} onclick={() => void send()}>
      {t('sendToPear.send', { count })}
    </Button>
  {/snippet}
</Modal>

<style>
  .send {
    display: flex;
    flex-direction: column;
    gap: var(--s-4);
    font-size: 12.5px;
  }
  .send p { margin: 0; line-height: 1.5 }
  .intro, .quiet { color: var(--t2) }
  .error { color: var(--warn) }

  .running {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-3);
    padding: var(--s-3) var(--s-4);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--panel);
  }

  .titles {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 320px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
  }
  .title, .again {
    display: flex;
    align-items: flex-start;
    gap: var(--s-3);
  }
  .title input, .again input { margin-top: 3px }
  .title.off { color: var(--t3) }
  label {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .name { color: var(--text); overflow-wrap: anywhere }
  .off .name { color: var(--t3) }
  .facts { font-size: 11.5px; color: var(--t3) }
  .tag {
    margin-left: var(--s-2);
    color: var(--t2);
  }
  .again {
    padding-top: var(--s-3);
    border-top: 1px solid var(--line);
  }
</style>
