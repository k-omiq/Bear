<script>
  /**
   * Setup step "Join" (`docs/shared-data-folder.md` section 5.5, start row
   * C): Pear already set up the shared data folder, and this is the cleaner's
   * first launch. Offered only then, before the model downloads.
   *
   * The two answers are the footer's (`FirstLaunchDialog.svelte`): "Use the
   * same folder" makes `Cleaner/` and its library file there (a new install
   * has nothing to move), "Keep separate" stores the answer and keeps the
   * library in its own place. Either way the question is not asked again.
   * This draws the question, and what the answer did.
   */
  import { t } from '../../i18n/index.js'
  import { dataFolder, startAnswers } from '../../state/datafolder.svelte.js'
  import { shownPath } from '../../state/startflow.js'

  const path = $derived(shownPath(dataFolder.status?.root, dataFolder.status?.home))
  const answer = $derived(startAnswers().join)
</script>

<p class="lead">{t('onboarding.join.body', { path })}</p>
{#if dataFolder.busy}
  <p class="note" role="status">{t('onboarding.join.working')}</p>
{:else if answer === 'accepted' && dataFolder.status?.mode === 'shared'}
  <p class="note" role="status">{t('onboarding.join.done', { path })}</p>
{:else if answer === 'declined'}
  <p class="note" role="status">{t('onboarding.join.kept')}</p>
{/if}
{#if dataFolder.error}
  <p class="alert" role="alert">{t('onboarding.join.failed', { detail: dataFolder.error })}</p>
{/if}
