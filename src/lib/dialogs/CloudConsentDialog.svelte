<script>
  /**
   * The one question asked before a project's first cloud render: three
   * facts, the two statements and a yes, which stands for the rest of the
   * project.
   *
   * What is sent (a crop around the region and its mask, never the page),
   * where it goes (the endpoint's saved name and its provider, or the provider
   * alone when it has no name, and its host), and what it costs (the
   * estimate, or plainly that there is none). Pushed by
   * `src/lib/editor/cloudflow.svelte.js#requestCloudConsent` once per project
   * and endpoint, with `blocking: true`: Escape is Cancel, the backdrop is inert, so
   * nothing but the confirming button sends anything. The grant is minted only
   * after that button, which is what makes this dialog the consent rather than
   * a courtesy.
   *
   * The confirming button is disabled until both statements are checked, as in
   * `CloudRunConsentDialog.svelte`. Neither is checked for the user. It
   * resolves with the two answers; the native side refuses a first consent
   * without them, and only a consent with both stands for the project, so a
   * later run or clean there never attests what was not ticked.
   *
   * Every value is fixed when the dialog mounts: the host keys the block by
   * modal id, and the props are the proposal the native side prepared.
   */
  import { untrack } from 'svelte'
  import { Button, Modal } from '../ui/index.js'
  import { closeModal, modalWidth } from '../state/app.svelte.js'
  import { t } from '../i18n/index.js'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()
  const uid = $props.id()

  const facts = untrack(() => /** @type {Record<string, unknown>} */ (spec?.props ?? {}))
  const width = Math.max(0, Math.round(Number(facts.width) || 0))
  const height = Math.max(0, Math.round(Number(facts.height) || 0))
  const name = String(facts.profileName ?? '').trim()
  const host = String(facts.host ?? '')
  const providerKey = facts.provider === 'beam' ? 'settings.inference.provider.beam' : 'settings.inference.provider.modal'
  const estimate = facts.estimatedCostUsd
  const cost = typeof estimate === 'number' && Number.isFinite(estimate) && estimate >= 0 ? estimate : null
  const titleKey = untrack(() => spec?.titleKey ?? 'modal.title.cloudConsent')
  const actions = untrack(() => spec?.actions ?? [])

  let rights = $state(false)
  let retention = $state(false)

  /** @param {string} id */
  function answer(id) {
    if (id === 'confirm') {
      if (!rights || !retention) return
      closeModal({ rightsAttested: rights, retentionAcknowledged: retention })
    } else {
      closeModal(id)
    }
  }
</script>

<Modal
  title={t(titleKey)}
  width={modalWidth('cloudConsent')}
  blocking
  onclose={() => closeModal(null)}
>
  <dl class="facts">
    <div class="fact">
      <dt>{t('modal.cloudConsent.what')}</dt>
      <dd>{t('modal.cloudConsent.whatValue', { width, height })}</dd>
    </div>
    <div class="fact">
      <dt>{t('modal.cloudConsent.where')}</dt>
      <dd>
        {name
          ? t('modal.cloudConsent.whereValue', { name, providerKey })
          : t('modal.cloudConsent.whereUnnamed', { providerKey })}
        {#if host}<span class="host">{host}</span>{/if}
      </dd>
    </div>
    <div class="fact">
      <dt>{t('modal.cloudConsent.cost')}</dt>
      <dd>
        {cost === null
          ? t('modal.cloudConsent.costUnknown')
          : t('modal.cloudConsent.costEstimate', { cost })}
      </dd>
    </div>
  </dl>
  <div class="answers">
    <div class="answer">
      <input id="{uid}-rights" type="checkbox" bind:checked={rights} />
      <label for="{uid}-rights">{t('cloud.analysis.rights')}</label>
    </div>
    <div class="answer">
      <input id="{uid}-retention" type="checkbox" bind:checked={retention} />
      <label for="{uid}-retention">{t('cloud.analysis.retention')}</label>
    </div>
  </div>
  <p class="project">{t('cloud.projectConsent')}</p>

  {#snippet buttons()}
    {#each actions as action (action.id)}
      <Button
        variant={action.variant ?? 'ghost'}
        disabled={action.id === 'confirm' && (!rights || !retention)}
        onclick={() => answer(action.id)}
      >
        {t(action.labelKey)}
      </Button>
    {/each}
  {/snippet}
</Modal>

<style>
  .facts {
    display: grid;
    gap: var(--s-4);
    margin: 0;
  }
  .fact {
    display: grid;
    gap: var(--s-1);
  }
  dt {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--t3);
  }
  dd {
    margin: 0;
    color: var(--text);
    line-height: 1.45;
  }
  .answers {
    display: grid;
    gap: 8px;
    margin-top: var(--s-4);
    padding-top: var(--s-4);
    border-top: 1px solid var(--line);
  }
  .answer {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 8px;
    align-items: start;
    font-size: 12px;
    line-height: 1.45;
    color: var(--text);
  }
  .answer input {
    margin: 2px 0 0;
    cursor: pointer;
  }
  .answer label {
    cursor: pointer;
  }
  .project {
    margin: var(--s-4) 0 0;
    font-size: 12px;
    color: var(--t2);
  }
  .host {
    display: block;
    margin-top: 2px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
    color: var(--t2);
    overflow-wrap: anywhere;
  }
</style>
