<script>
  /**
   * The consent in front of a Detect and clean whose clean runs on the cloud
   * GPU page by page: which pages, where, on which GPU at what price, and the
   * spending cap. Pushed by `src/lib/editor/cloudrun.js#askStream` with
   * `blocking: true`, so Escape is Cancel and the backdrop is inert.
   *
   * No region is known yet, so the consent is for the pages and a cap, not a
   * plan: each page's regions are sent as soon as the page is detected, and
   * the run stops sending when the app's own estimate of what it has sent
   * reaches the cap. The dialog says plainly that the provider bills actual
   * use, which the estimate only approaches.
   *
   * The same two statements as every cloud consent, and the same rule: Start
   * is disabled until both are checked, and neither is checked for the user.
   * A project whose standing consent covers them (`standing`) hides them;
   * the cap is asked every time. Start is also disabled until the cap is a
   * number from the fixed cost to the maximum. Start resolves with the
   * answers, the stream digest shown, and the cap; the native side mints the
   * run's grant only for that digest and that cap.
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
  const titleKey = untrack(() => spec?.titleKey ?? 'cloud.clean.stream.title')
  const actions = untrack(() => spec?.actions ?? [])

  const standing = proposal.standing === true
  let rights = $state(standing)
  let retention = $state(standing)

  const money = (value) => (typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null)
  const pages = Number.isFinite(proposal.pages) ? Number(proposal.pages) : 0
  const mixed = proposal.execution === 'mixed'
  const fixed = money(proposal.fixedCostUsd) ?? 0
  const region = money(proposal.regionCostUsd)
  const max = money(proposal.maxCapUsd) ?? 0
  const perHour = money(proposal.gpuUsdPerHour)
  const digest = typeof proposal.streamDigest === 'string' ? proposal.streamDigest : ''
  const expires = Number.isFinite(proposal.expiresAtMs) && proposal.expiresAtMs > 0
    ? new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }).format(proposal.expiresAtMs)
    : null

  // In cents: the field is what the person types, not the estimate. A
  // number field binds a number, or null while it is empty.
  let cap = $state(/** @type {number|null} */ (Math.round((money(proposal.suggestedCapUsd) ?? fixed) * 100) / 100))
  const capValid = $derived(typeof cap === 'number' && Number.isFinite(cap) && cap >= fixed && cap <= max)
  const ready = $derived(capValid && rights && retention)

  /** @param {string} id */
  function answer(id) {
    if (id === 'confirm') {
      if (!ready) return
      // A standing project's statements are the native side's to supply.
      closeModal({
        rightsAttested: standing ? false : rights,
        retentionAcknowledged: standing ? false : retention,
        streamDigest: digest,
        capUsd: cap,
      })
    } else {
      closeModal(id)
    }
  }
</script>

<Modal title={t(titleKey)} width={520} blocking onclose={() => closeModal(null)}>
  <div class="consent" data-proposal={proposal.proposalId}>
    <p class="heading">
      <Icon name="cloud" size={14} />
      {t('cloud.clean.stream.heading', { pages: t('cloud.analysis.pages', { count: pages }) })}
    </p>

    <dl class="facts">
      <div class="fact">
        <dt>{t('cloud.analysis.run.what')}</dt>
        <dd data-fact="what">{t('cloud.clean.stream.whatValue')}</dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.clean.execution')}</dt>
        <dd data-fact="execution" data-execution={mixed ? 'mixed' : 'cloud'}>
          {#if mixed}
            <span>{t('cloud.clean.mixed.label')}</span>
            <span class="quiet">{t('cloud.clean.stream.executionMixed')}</span>
          {:else}
            <span>{t('cloud.clean.executionCloud')}</span>
          {/if}
        </dd>
      </div>
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
        <dd data-fact="gpu">
          {perHour === null ? proposal.gpu : t('cloud.clean.stream.price', { gpu: proposal.gpu, price: perHour })}
        </dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.analysis.consent.cost')}</dt>
        <dd data-fact="cost">
          {#if region !== null}<span>{t('cloud.clean.stream.costValue', { region, fixed })}</span>{/if}
          <span class="quiet">{t('cloud.clean.costBasis')}</span>
        </dd>
      </div>
      <div class="fact">
        <dt><label for="{uid}-cap">{t('cloud.clean.stream.cap')}</label></dt>
        <dd>
          <span class="cap">
            <span aria-hidden="true">$</span>
            <input id="{uid}-cap" data-fact="cap" type="number" inputmode="decimal" step="0.1" min={fixed} max={max}
              bind:value={cap} aria-invalid={!capValid} aria-describedby="{uid}-cap-help" />
          </span>
          <span id="{uid}-cap-help" class:warn={!capValid} class="quiet">
            {capValid ? t('cloud.clean.stream.capHelp') : t('cloud.clean.stream.capInvalid', { min: fixed, max })}
          </span>
        </dd>
      </div>
      <div class="fact">
        <dt>{t('cloud.analysis.run.result')}</dt>
        <dd>
          <span>{t('cloud.clean.stream.resultValue')}</span>
          <span class="quiet">{t('cloud.clean.stream.checks')}</span>
          {#if digest}
            <details class="identity">
              <summary>{t('cloud.clean.plan')}</summary>
              <code data-fact="plan">{digest}</code>
            </details>
          {/if}
        </dd>
      </div>
    </dl>

    {#if !standing}
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
    {/if}
    {#if expires}<p class="expires">{t('cloud.analysis.consent.expires', { time: expires })}</p>{/if}
  </div>

  {#snippet buttons()}
    {#each actions as action (action.id)}
      <Button
        variant={action.variant ?? 'ghost'}
        disabled={action.id === 'confirm' && !ready}
        onclick={() => answer(action.id)}
      >
        {#if action.id === 'confirm'}<Icon name="cloud" size={12} />{/if}{t(action.labelKey)}
      </Button>
    {/each}
  {/snippet}
</Modal>

<style>
  /* The plan consent's layout, fact for fact (`CloudCleanConsentDialog`). */
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
  .warn { color: var(--warn) }
  .identity { font-size: 11.5px; color: var(--t2) }
  .identity > summary { cursor: pointer; width: max-content }
  code { font-size: 11px; color: var(--t2); overflow-wrap: anywhere }

  .cap { display: inline-flex; align-items: center; gap: 4px; width: max-content }
  .cap input { width: 96px; font: inherit; font-variant-numeric: tabular-nums }

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
