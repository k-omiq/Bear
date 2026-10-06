<script>
  /**
   * The consent step of a cloud analysis: exactly what the native side
   * proposed to send, where, with which model, at what cost, and what comes
   * back. Nothing leaves the computer until Send, and Send is disabled until
   * both statements are checked. Neither is checked for the user, and neither
   * is remembered: every proposal asks again.
   *
   * Presentational. Every value is the proposal's own
   * (`propose_remote_analysis`); the owner, `CloudAnalysis.svelte`, confirms
   * with the two answers or discards the proposal on Cancel.
   */
  import { onMount } from 'svelte'
  import { t } from '../i18n/index.js'
  import { Button } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'

  /**
   * @type {{
   *   proposal: any,
   *   pageNumber: number,
   *   capabilityKey: string,
   *   keyOf?: (capability: string) => string,
   *   providerKey: string,
   *   busy?: boolean,
   *   onconfirm: (answers: { rightsAttested: boolean, retentionAcknowledged: boolean }) => void,
   *   oncancel: () => void,
   * }}
   */
  let { proposal, pageNumber, capabilityKey, keyOf = () => capabilityKey, providerKey, busy = false, onconfirm, oncancel } = $props()
  const uid = $props.id()

  let rights = $state(false)
  let retention = $state(false)
  /** @type {HTMLElement | null} */
  let heading = $state(null)
  /** @type {HTMLElement | null} */
  let section = $state(null)

  const numbers = new Intl.NumberFormat()
  const tiles = $derived(proposal.tiles?.length ?? 0)
  // A negative figure is a sentinel, not a price (as in CloudConsentDialog).
  const cost = $derived(typeof proposal.costEstimateUsd === 'number' && Number.isFinite(proposal.costEstimateUsd)
    && proposal.costEstimateUsd >= 0 ? proposal.costEstimateUsd : null)
  const graphs = $derived(Array.isArray(proposal.graphSha256s) ? proposal.graphSha256s : [])
  /** Every model the tiles go to: more than one when the review asked for a companion. */
  const companions = $derived(Array.isArray(proposal.models) && proposal.models.length > 1 ? proposal.models : null)
  const expires = $derived(Number.isFinite(proposal.expiresAtMs)
    ? new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }).format(proposal.expiresAtMs)
    : null)

  /** A hash as a person compares it at a glance; the full value is one click away. */
  function short(value) {
    const text = String(value ?? '')
    return text.length > 12 ? `${text.slice(0, 12)}…` : text
  }

  /**
   * Escape backs out of this step and no further. The review around it is a
   * `Modal`, which closes on an Escape that reaches `window`; that would
   * throw away the local analysis along with the proposal. So the key is
   * taken on `document`, which it reaches first, and from anywhere in the
   * review: a press on plain text leaves focus on the page in WebKit,
   * outside this section. A menu or popover inside the review claims its own
   * Escape before then.
   *
   * An Escape pressed in another dialog is that dialog's. With focus on
   * nothing it belongs to the topmost dialog, which is the last one in the
   * document, so the step takes it only when that is the review.
   *
   * @param {KeyboardEvent} event
   */
  function onescape(event) {
    if (event.key !== 'Escape' || !section) return
    // The review's dialog, or the step itself where it stands alone.
    const host = section.closest('[role="dialog"]') ?? section
    const target = event.target
    const nowhere = target === document || target === document.body || target === document.documentElement
    if (nowhere) {
      const dialogs = document.querySelectorAll('[aria-modal="true"]')
      const topmost = dialogs.length > 0 ? dialogs[dialogs.length - 1] : null
      if (topmost && topmost !== host) return
    } else if (!(target instanceof Node) || !host.contains(target)) return
    event.preventDefault()
    event.stopPropagation()
    if (!busy) oncancel()
  }

  onMount(() => {
    heading?.focus()
    document.addEventListener('keydown', onescape)
    return () => document.removeEventListener('keydown', onescape)
  })
</script>

<section class="consent" aria-labelledby="{uid}-heading" data-proposal={proposal.proposalId} bind:this={section}>
  <h4 id="{uid}-heading" tabindex="-1" bind:this={heading}>
    <Icon name="cloud" size={14} />{t('cloud.analysis.consent.heading', { page: pageNumber })}
  </h4>

  <dl class="facts">
    <div class="fact">
      <dt>{t('cloud.analysis.consent.what')}</dt>
      <dd>
        {t('cloud.analysis.disclosure', {
          pages: t('cloud.analysis.pages', { count: proposal.pages ?? 1 }),
          tiles: t('cloud.analysis.tiles', { count: tiles }),
          pixels: numbers.format(proposal.totalTilePixels ?? 0),
          bytes: numbers.format(proposal.totalEncodedBytes ?? 0),
          size: proposal.totalEncodedBytes ?? 0,
        })}
      </dd>
    </div>
    <div class="fact">
      <dt>{t('cloud.analysis.consent.where')}</dt>
      <dd>{t('cloud.analysis.consent.whereValue', { name: proposal.profileName || proposal.profileId, providerKey })}</dd>
    </div>
    <div class="fact">
      <dt>{t('cloud.analysis.consent.model')}</dt>
      <dd>
        {#if companions}
          {#each companions as model (model.capability)}
            <span>{t('cloud.analysis.consent.modelValue', { capabilityKey: keyOf(model.capability), revision: short(model.modelRevision) })}</span>
          {/each}
          <span class="quiet">{t('cloud.analysis.consent.eachModel')}</span>
        {:else}
          <span>{t('cloud.analysis.consent.modelValue', { capabilityKey, revision: short(proposal.modelRevision) })}</span>
          {#if graphs.length}
            <span class="quiet">{t('cloud.analysis.consent.graphs', { graphs: graphs.map(short).join(', ') })}</span>
          {/if}
        {/if}
        <details class="identity">
          <summary>{t('cloud.analysis.consent.identity')}</summary>
          <dl>
            {#each companions ?? [{ modelRevision: proposal.modelRevision, graphSha256s: graphs }] as model, slot (slot)}
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
      <dt>{t('cloud.analysis.consent.result')}</dt>
      <dd>{t('cloud.analysis.reviewOnly')}</dd>
    </div>
  </dl>

  <div class="answers">
    <div class="answer">
      <input id="{uid}-rights" type="checkbox" bind:checked={rights} disabled={busy} />
      <label for="{uid}-rights">{t('cloud.analysis.rights')}</label>
    </div>
    <div class="answer">
      <input id="{uid}-retention" type="checkbox" bind:checked={retention} disabled={busy} />
      <label for="{uid}-retention">{t('cloud.analysis.retention')}</label>
    </div>
  </div>

  <div class="footer">
    {#if expires}<p class="expires">{t('cloud.analysis.consent.expires', { time: expires })}</p>{/if}
    <div class="buttons">
      <Button disabled={busy} onclick={oncancel}>{t('cloud.analysis.consent.cancel')}</Button>
      <Button
        variant="primary"
        disabled={busy || !rights || !retention}
        onclick={() => onconfirm({ rightsAttested: rights, retentionAcknowledged: retention })}
      >
        <Icon name="cloud" size={12} />{t('cloud.analysis.consent.confirm')}
      </Button>
    </div>
  </div>
</section>

<style>
  .consent {
    display: grid;
    gap: var(--s-4);
    padding: 12px 14px;
    border: 1px solid var(--line2);
    border-radius: var(--r-md);
    background: var(--surface);
  }
  h4 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text);
  }
  h4:focus { outline: none }
  h4:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: var(--r-xs) }

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

  .footer { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--s-3) }
  .expires { margin: 0; font-size: 11px; color: var(--t3) }
  .buttons { display: flex; gap: var(--s-2); margin-left: auto }

  @container (max-width: 520px) {
    .fact { grid-template-columns: minmax(0, 1fr) }
  }
</style>
