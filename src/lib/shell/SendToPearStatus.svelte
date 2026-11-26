<script>
  /**
   * "Sending 40 of 63 chapters", with Stop, while a Send to Pear runs
   * (`docs/shared-data-folder.md` section 8.4.1). It sits in the shell's
   * bottom-left corner on every screen, because the send outlives the title
   * list that started it. Stop ends it after the chapter being written; the
   * messages already written stay valid.
   */
  import { Button } from '../ui/index.js'
  import { sendRunning, sendToPear, stopSend } from '../state/sendtopear.svelte.js'
  import { t } from '../i18n/index.js'

  const progress = $derived(sendToPear.progress)
</script>

{#if sendRunning() && progress}
  <div class="status" role="status" aria-live="polite">
    <span>{t('sendToPear.sending', { done: progress.done, total: progress.total })}</span>
    <Button size="sm" onclick={() => void stopSend()}>{t('sendToPear.stop')}</Button>
  </div>
{/if}

<style>
  .status {
    display: flex;
    align-items: center;
    gap: var(--s-4);
    padding: var(--s-3) var(--s-4);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--modal-lift);
    font-size: 12px;
    color: var(--text);
  }
</style>
