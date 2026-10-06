<script>
  /**
   * The consent in front of an Auto clean whose detection runs on the cloud
   * GPU: what is sent, where, with which models, at what cost, and what
   * happens after. Pushed by `src/lib/editor/cloudrun.js#startCleanRun` with
   * `blocking: true`, so Escape is Cancel and the backdrop is inert.
   *
   * Send is disabled until both statements are checked. Neither is checked
   * for the user. Send resolves with the two answers; the native side mints
   * the run's grant only after that, which is what makes this the consent
   * and not a notice. It is also the project's standing consent: later
   * requests in the project to this endpoint do not ask again (`cloudrun.js`).
   *
   * Every value is the proposal's own (`propose_run_analysis`), fixed when the
   * dialog mounts. With Clean on the cloud too (`cleanFollows`), it says that
   * the cleaning that follows detection goes to the cloud GPU under the same
   * consent.
   */
  import { untrack } from 'svelte'
  import { Button, Modal } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { t } from '../i18n/index.js'
  import Icon from '../icons/Icon.svelte'
  import { capabilityKeyOf, providerKeyOf } from './CloudAnalysis.svelte'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()
  const uid = $props.id()

  const proposal = untrack(() => /** @type {any} */ (spec?.props?.proposal ?? {}))
  const titleKey = untrack(() => spec?.titleKey ?? 'cloud.analysis.run.title')
  const chapterRun = untrack(() => spec?.props?.scope === 'chapter')
  const cleanFollows = untrack(() => spec?.props?.cleanFollows === true)
  const actions = untrack(() => spec?.actions ?? [])

  let rights = $state(false)
  let retention = $state(false)

  const numbers = new Intl.NumberFormat()
  const indices = Array.isArray(proposal.pageIndices) ? proposal.pageIndices : []
  const pageNumber = (() => {
    const index = indices[0] ?? 0
    return editor.chapter?.pages?.find((page) => page.index === index)?.number ?? index + 1
  })()
  const models = Array.isArray(proposal.models) ? proposal.models : []
  // A negative figure is a sentinel, not a price (as in CloudConsentDialog).
  const cost = typeof proposal.costEstimateUsd === 'number' && Number.isFinite(proposal.costEstimateUsd)
    && proposal.costEstimateUsd >= 0 ? proposal.costEstimateUsd : null
  const expires = Number.isFinite(proposal.expiresAtMs)
    ? new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }).format(proposal.expiresAtMs)
    : null

  /** A hash as a person compares it at a glance; the full value is one click away. */
  function short(value) {
    const text = String(value ?? '')
    return text.length > 12 ? `${text.slice(0, 12)}…` : text
  }

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

<Modal title={t(titleKey)} width={520} blocking onclose={() => closeModal(null)}>
  <div class="consent" data-proposal={proposal.proposalId}>
    <p class="heading">
      <Icon name="cloud" size={14} />
      {chapterRun
        ? t('cloud.analysis.run.headingChapter', { pages: t('cloud.analysis.pages', { count: proposal.pages ?? indices.length }) })
        : t('cloud.analysis.run.heading', { page: pageNumber })}
    </p>

    <dl class="facts">
      <div class="fact">
        <dt>{t('cloud.analysis.run.what')}</dt>
        <dd>
          {t('cloud.analysis.run.whatValue', {
            pages: t('cloud.analysis.pages', { count: proposal.pages ?? indices.length }),
            tiles: t('cloud.analysis.tiles', { count: proposal.totalTiles ?? 0 }),
            pixels: numbers.format(proposal.totalTilePixels ?? 0),
          })}
        </dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.analysis.run.where')}</dt>
        <dd>{t('cloud.analysis.consent.whereValue', {
          name: proposal.profileName || proposal.profileId,
          providerKey: providerKeyOf(proposal.provider),
        })}</dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.analysis.run.models')}</dt>
        <dd>
          {#each models as model (model.capability)}
            <span>{t('cloud.analysis.consent.modelValue', {
              capabilityKey: capabilityKeyOf(model.capability),
              revision: short(model.modelRevision),
            })}</span>
          {/each}
          {#if models.length > 1}<span class="quiet">{t('cloud.analysis.consent.eachModel')}</span>{/if}
          <details class="identity">
            <summary>{t('cloud.analysis.consent.identity')}</summary>
            <dl>
              {#each models as model (model.capability)}
                <dt>{t('cloud.analysis.consent.revision')}</dt><dd><code>{model.modelRevision}</code></dd>
                {#each model.graphSha256s ?? [] as graph, index (index)}
                  <dt>{t('cloud.analysis.consent.graph')}</dt><dd><code>{graph}</code></dd>
                {/each}
              {/each}
            </dl>
          </details>
        </dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.analysis.consent.cost')}</dt>
        <dd>
          <span data-cost={cost === null ? 'unknown' : 'estimate'}>
            {cost === null ? t('cloud.analysis.costUnknown') : t('cloud.analysis.costEstimate', { cost })}
          </span>
          <span class="quiet">{t('cloud.analysis.costNote')}</span>
        </dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.analysis.run.result')}</dt>
        <dd>
          {#if cleanFollows}
            <span data-fact="clean-follows">{t('cloud.clean.asksAfterDetection')}</span>
          {:else}
            <span>{t('cloud.analysis.run.resultValue')}</span>
          {/if}
          <span class="quiet">{t(chapterRun ? 'cloud.analysis.run.scopeChapter' : 'cloud.analysis.run.scope')}</span>
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

    <p class="project" data-fact="project">{t('cloud.projectConsent')}</p>
    {#if expires}<p class="expires">{t('cloud.analysis.consent.expires', { time: expires })}</p>{/if}
  </div>

  {#snippet buttons()}
    {#each actions as action (action.id)}
      <Button
        variant={action.variant ?? 'ghost'}
        disabled={action.id === 'confirm' && (!rights || !retention)}
        onclick={() => answer(action.id)}
      >
        {#if action.id === 'confirm'}<Icon name="cloud" size={12} />{/if}{t(action.labelKey)}
      </Button>
    {/each}
  {/snippet}
</Modal>

<style>
  .consent { display: grid; gap: var(--s-4) }
  .heading {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text);
  }

  .facts { display: grid; gap: 8px; margin: 0 }
  .fact { display: grid; grid-template-columns: 112px minmax(0, 1fr); gap: 2px 12px; align-items: baseline }
  .fact > dt {
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: .04em;
    text-transform: uppercase;
    color: var(--t3);
  }
  .fact > dd { display: grid; gap: 2px; margin: 0; min-width: 0; font-size: 12px; line-height: 1.45; color: var(--text); overflow-wrap: anywhere }
  .quiet { color: var(--t2) }
  .identity { font-size: 11.5px; color: var(--t2) }
  .identity > summary { cursor: pointer; width: max-content }
  .identity dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: 3px 10px; margin: 6px 0 0 }
  .identity dt { color: var(--t3) }
  .identity dd { margin: 0; min-width: 0 }
  code { font-size: 11px; color: var(--t2); overflow-wrap: anywhere }

  .answers { display: grid; gap: 8px; padding-top: var(--s-4); border-top: 1px solid var(--line) }
  .answer { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px; align-items: start; font-size: 12px; line-height: 1.45; color: var(--text) }
  .answer input { margin: 2px 0 0; cursor: pointer }
  .answer label { cursor: pointer }

  .project { margin: 0; font-size: 12px; color: var(--t2) }
  .expires { margin: 0; font-size: 11px; color: var(--t3) }

  @media (max-width: 520px) {
    .fact { grid-template-columns: minmax(0, 1fr) }
  }
</style>
