<script>
  /**
   * The consent in front of a cloud clean: which detected regions go to the
   * cloud GPU, whether flat colours are tried on this computer first, the
   * batches they are sent in, any region held back by an unresolved earlier
   * request, where they go, on which GPU, at what GPU time and cost range, and
   * what happens to a region that fails. One consent covers the whole plan;
   * the plan's digest is what it is given for. Pushed by
   * `src/lib/editor/cloudrun.js` with `blocking: true`, so Escape is Cancel
   * and the backdrop is inert.
   *
   * The same two statements as the run consent, and the same rule: Confirm is
   * disabled until both are checked, and neither is checked for the user.
   * Confirm resolves with the two answers; the native side mints the batch's
   * grant only after that, which is what makes this the consent and not a
   * notice. It is also the project's standing consent: later requests in the
   * project to this endpoint do not ask again (`cloudrun.js`).
   *
   * Every value is the proposal's own (`prepare_cloud_clean`), fixed when the
   * dialog mounts, except the endpoint's address, which is the readiness the
   * flow read just before preparing.
   */
  import { untrack } from 'svelte'
  import { Button, Modal } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { t } from '../i18n/index.js'
  import Icon from '../icons/Icon.svelte'
  import { providerKeyOf } from './CloudAnalysis.svelte'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()
  const uid = $props.id()

  const proposal = untrack(() => /** @type {any} */ (spec?.props?.proposal ?? {}))
  const endpoint = untrack(() => /** @type {string|null} */ (spec?.props?.endpoint ?? null))
  const titleKey = untrack(() => spec?.titleKey ?? 'cloud.clean.title')
  const actions = untrack(() => spec?.actions ?? [])

  let rights = $state(false)
  let retention = $state(false)

  const numbers = new Intl.NumberFormat()
  const count = (value) => (Array.isArray(value) ? value.length : Number.isFinite(value) ? Number(value) : 0)
  const regions = count(proposal.regions ?? proposal.regionIds)
  const pages = count(proposal.pages)
  const mixed = proposal.execution === 'mixed'
  const localCandidates = count(proposal.localCandidates)
  const chunks = Math.max(1, count(proposal.chunks))
  const chunkSize = count(proposal.chunkRegions) || regions
  const held = count(proposal.unresolvedIds)
  const tooLarge = count(proposal.tooLargeIds)
  const seconds = proposal.estimatedGpuSeconds
  const minutes = seconds && Number.isFinite(seconds.low) && Number.isFinite(seconds.high) && seconds.high >= seconds.low
    ? { low: Math.max(1, Math.round(seconds.low / 60)), high: Math.max(1, Math.round(seconds.high / 60)) }
    : null
  const digest = typeof proposal.planDigest === 'string' ? proposal.planDigest : ''
  // A range the native side could price, or nothing: a guessed figure would
  // be worse than saying the price is unknown.
  const range = proposal.estimatedCostUsd
  const cost = range && Number.isFinite(range.low) && Number.isFinite(range.high) && range.low >= 0 && range.high >= range.low
    ? { low: range.low, high: range.high }
    : null
  // 0 is the native side's "no proposal", never a time to show.
  const expires = Number.isFinite(proposal.expiresAtMs) && proposal.expiresAtMs > 0
    ? new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }).format(proposal.expiresAtMs)
    : null

  /** @param {string} id */
  function answer(id) {
    if (id === 'confirm') {
      if (!rights || !retention) return
      // The plan shown is the plan agreed to: the native side mints a grant
      // only for this digest.
      closeModal({ rightsAttested: rights, retentionAcknowledged: retention, planDigest: digest })
    } else {
      closeModal(id)
    }
  }
</script>

<Modal title={t(titleKey)} width={520} blocking onclose={() => closeModal(null)}>
  <div class="consent" data-proposal={proposal.proposalId}>
    <p class="heading">
      <Icon name="cloud" size={14} />
      {t('cloud.clean.heading', { regions: t('cloud.clean.regions', { count: regions }) })}
    </p>

    <dl class="facts">
      <div class="fact">
        <dt>{t('cloud.analysis.run.what')}</dt>
        <dd data-fact="what">
          {t('cloud.clean.whatValue', {
            regions: t('cloud.clean.regions', { count: regions }),
            pages: t('cloud.analysis.pages', { count: pages }),
            pixels: numbers.format(proposal.totalCropPixels ?? 0),
          })}
        </dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.clean.execution')}</dt>
        <dd data-fact="execution" data-execution={mixed ? 'mixed' : 'cloud'}>
          {#if mixed}
            <span>{t('cloud.clean.mixed.label')}</span>
            <span class="quiet">{t('cloud.clean.executionMixed', { count: localCandidates })}</span>
          {:else}
            <span>{t('cloud.clean.executionCloud')}</span>
          {/if}
        </dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.clean.batches')}</dt>
        <dd data-fact="batches">
          <span>{t('cloud.clean.batchesValue', {
            batches: t('cloud.clean.batchCount', { count: chunks }),
            size: numbers.format(chunkSize),
          })}</span>
          <span class="quiet">{t('cloud.clean.batchesNote')}</span>
        </dd>
      </div>
      {#if held > 0}
        <div class="fact">
          <dt>{t('cloud.clean.heldBack')}</dt>
          <dd data-fact="held">{t('cloud.clean.heldBackValue', { count: held })}</dd>
        </div>
      {/if}
      {#if tooLarge > 0}
        <div class="fact">
          <dt>{t('cloud.clean.tooLarge')}</dt>
          <dd data-fact="tooLarge">{t('cloud.clean.tooLargeValue', { count: tooLarge })}</dd>
        </div>
      {/if}
      <div class="fact">
        <dt>{t('cloud.analysis.run.where')}</dt>
        <dd data-fact="where">
          <span>{t('cloud.analysis.consent.whereValue', {
            name: proposal.profileName || proposal.profileId,
            providerKey: providerKeyOf(proposal.provider),
          })}</span>
          {#if endpoint}<span class="quiet"><code>{endpoint}</code></span>{/if}
        </dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.clean.gpu')}</dt>
        <dd data-fact="gpu">{proposal.gpu || t('cloud.clean.gpuUnknown')}</dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.analysis.consent.cost')}</dt>
        <dd>
          <span data-cost={cost === null ? 'unknown' : 'estimate'}>
            {cost === null ? t('cloud.analysis.costUnknown') : t('cloud.clean.costRange', cost)}
          </span>
          {#if minutes}<span class="quiet" data-fact="gpu-time">{t('cloud.clean.gpuTime', minutes)}</span>{/if}
          <span class="quiet">{t('cloud.clean.costBasis')}</span>
        </dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.analysis.run.result')}</dt>
        <dd>
          <span>{t('cloud.clean.resultValue')}</span>
          <span class="quiet">{t('cloud.clean.scope')}</span>
          {#if digest}
            <details class="identity">
              <summary>{t('cloud.clean.plan')}</summary>
              <code data-fact="plan">{digest}</code>
            </details>
          {/if}
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
  /* The run consent's layout, fact for fact, so the two cloud questions read
     as one kind of question. */
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
