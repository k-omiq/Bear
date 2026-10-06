<script module>
  /**
   * The analysis models a cloud GPU can be asked for from the review, in the
   * order offered. The SFX finder (COO) is never one of them: its rights are
   * unresolved, locally and remotely.
   */
  export const CLOUD_CAPABILITIES = Object.freeze([
    { id: 'text_mask_sam_ts@1', key: 'cloud.analysis.capability.sam' },
    { id: 'text_regions_rt@1', key: 'cloud.analysis.capability.rt' },
  ])

  /**
   * A capability's name, or the pair's when a review sent both (`a+b`).
   *
   * @param {string|null|undefined} id
   */
  export function capabilityKeyOf(id) {
    if (typeof id === 'string' && id.includes('+')) return 'cloud.analysis.capability.both'
    return CLOUD_CAPABILITIES.find((entry) => entry.id === id)?.key ?? 'cloud.analysis.capability.sam'
  }

  /** @param {string|null|undefined} provider */
  export function providerKeyOf(provider) {
    return provider === 'beam' ? 'settings.inference.provider.beam' : 'settings.inference.provider.modal'
  }
</script>

<script>
  /**
   * Cloud analysis of the review's current page, on the user's own Modal or
   * Beam endpoint.
   *
   * Offered only when cloud engines are allowed and the selected target is a
   * cloud endpoint with a stored key: the shared readiness in
   * `state/cloud.svelte.js`, so it agrees with every other cloud control.
   * Otherwise it is a disabled strip saying what is missing, with the way to
   * Settings > Cloud. A long strip is refused before anything is asked:
   * cloud analysis is paginated only.
   *
   * The review's own backend choice can route Analyze here (`request`): the
   * owner then names the capability, and a companion when the workflow needs
   * both models, and calls `begin`. The picker and entry step aside for it.
   *
   * The flow never sends a pixel before consent. Pressing the entry asks the
   * endpoint which models it offers (no page data), then asks the native side
   * to propose exactly what it would send. The proposal is shown for consent;
   * Cancel there discards it. Confirm sends the tiles one by one, with a
   * running count and a Cancel. Nothing retries on its own: a stale page, a
   * cancelled run, or a tile whose fate is unknown each end in a named
   * outcome, and the user starts again if they want to.
   *
   * The result goes to `onresult`. It is review-only evidence: the native side
   * refuses to prepare a write from it, and the review says so.
   */
  import { onDestroy, onMount, tick, untrack } from 'svelte'
  import { getBackend } from '../api/backend.js'
  import { cloud, cloudUsable, openCloudSettings, refreshCloudReadiness } from '../state/cloud.svelte.js'
  import { editor } from '../state/editor.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { t } from '../i18n/index.js'
  import { Button } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'
  import CloudAnalysisConsent from './CloudAnalysisConsent.svelte'
  import WorkflowOutcome from './WorkflowOutcome.svelte'
  import { outcomeOf, outcomeOfRecord } from './workflowoutcome.js'

  /**
   * @type {{
   *   chapterId: string,
   *   pageIndex: number,
   *   pageNumber: number,
   *   locked?: boolean,
   *   request?: { capability: string, companion: string|null } | null,
   *   onactive?: (active: boolean) => void,
   *   onresult?: (analysis: any, meta: { provider: string, profileName: string, capability: string }) => void,
   * }}
   */
  let { chapterId, pageIndex, pageNumber, locked = false, request = null, onactive = () => {}, onresult = () => {} } = $props()
  const uid = $props.id()

  const readiness = $derived(cloud.readiness)
  const usable = $derived(cloudUsable())
  const longstrip = $derived(editor.project?.mode === 'longstrip')
  /** Why the strip is disabled, as a key, or null when it is not. Silent until readiness was read. */
  const unavailableKey = $derived(usable || !cloud.checked ? null
    : !session.cloudAllowed || readiness.reason === 'off' ? 'cloud.analysis.unavailable.off'
    : readiness.reason === 'noTarget' ? 'cloud.analysis.unavailable.noTarget'
    : readiness.reason === 'noSecret' ? 'cloud.analysis.unavailable.noSecret'
    : readiness.reason === 'secretLocked' ? 'cloud.analysis.unavailable.secretLocked'
    : 'cloud.analysis.unavailable.notReady')
  let capability = $state(CLOUD_CAPABILITIES[0].id)
  /** Models the endpoint said it does not offer, marked in the picker once known. */
  let unoffered = $state(new Set())
  /** @type {'idle'|'checking'|'proposing'|'consent'|'running'|'cancelling'} */
  let stage = $state('idle')
  let proposal = $state(null)
  /** @type {{done: number, total: number} | null} */
  let progress = $state(null)
  /** @type {import('./workflowoutcome.js').Outcome | null} */
  let outcome = $state(null)
  /** @type {HTMLElement | null} */
  let actions = $state(null)
  /** The newest journal record for the running proposal, for the outcome if status cannot be read. */
  let lastRecord = null
  /** @type {null | (() => void)} */
  let unlisten = null
  let attempt = 0
  let destroyed = false

  const active = $derived(stage !== 'idle')
  const profileName = $derived(String(readiness?.profile?.name || readiness?.target?.profile_id || ''))
  const providerKey = $derived(providerKeyOf(readiness?.target?.type))
  const percentDone = $derived(progress?.total ? Math.round(progress.done / progress.total * 100) : 0)

  $effect(() => {
    const value = active
    untrack(() => onactive(value))
  })

  // An outcome belongs to the page it was about.
  $effect(() => {
    void pageIndex
    untrack(() => { if (stage === 'idle') outcome = null })
  })

  onMount(() => {
    const backend = getBackend()
    Promise.resolve(backend.onRemoteAnalysis?.(onRecord))
      .then((off) => {
        if (typeof off !== 'function') return
        if (destroyed) off()
        else unlisten = off
      })
      .catch(() => {})
    if (!cloud.checked) refreshCloudReadiness(backend).catch(() => {})
  })

  onDestroy(() => {
    destroyed = true
    unlisten?.()
    // Closing the review is not a reason to keep sending tiles, nor to leave
    // a proposal open for the user to forget.
    if (proposal) discard(proposal.proposalId)
  })

  /** @param {any} record - a snake_case journal record from `cloud://analysis` */
  function onRecord(record) {
    if (!proposal || record?.proposal_id !== proposal.proposalId) return
    lastRecord = record
    if (Number.isInteger(record.total_tiles)) {
      progress = { done: Number(record.completed_tiles) || 0, total: record.total_tiles }
    }
  }

  /** @param {string} proposalId */
  function discard(proposalId) {
    Promise.resolve()
      .then(() => getBackend().cancelRemoteAnalysis({ proposalId }))
      .catch(() => {})
  }

  /** Return focus to the entry when the control that held it went away. */
  async function settleFocus() {
    await tick()
    const holder = document.activeElement
    if (holder && holder !== document.body && holder.isConnected && !holder.matches?.(':disabled')) return
    actions?.querySelector('[data-role="cloud-start"]')?.focus()
  }

  export async function begin() {
    if (stage !== 'idle' || locked || !chapterId || longstrip) return
    const run = ++attempt
    const backend = getBackend()
    const wanted = request ?? { capability, companion: null }
    outcome = null
    stage = 'checking'
    let phase = 'capabilities'
    try {
      const verdict = await refreshCloudReadiness(backend)
      if (run !== attempt || destroyed) return
      if (!verdict.ready || !verdict.target || !cloudUsable()) {
        stage = 'idle'
        return
      }
      const { type: provider, profile_id: profileId } = verdict.target
      const listed = await backend.listRemoteAnalysisCapabilities({ provider, profileId })
      if (run !== attempt || destroyed) return
      const offered = new Set((listed?.capabilities ?? []).map((entry) => entry?.capability))
      unoffered = new Set(CLOUD_CAPABILITIES.map((entry) => entry.id).filter((id) => !offered.has(id)))
      if (!offered.has(wanted.capability) || (wanted.companion && !offered.has(wanted.companion))) {
        outcome = { kind: 'capabilityMissing' }
        stage = 'idle'
        await settleFocus()
        return
      }
      phase = 'propose'
      stage = 'proposing'
      const next = await backend.proposeRemoteAnalysis({
        chapterId, pageIndex, provider, profileId, capability: wanted.capability,
        ...(wanted.companion ? { companion: wanted.companion } : {}),
      })
      if (run !== attempt || destroyed) {
        if (next?.proposalId) discard(next.proposalId)
        return
      }
      lastRecord = null
      proposal = next
      stage = 'consent'
    } catch (cause) {
      if (run !== attempt || destroyed) return
      outcome = outcomeOf(cause, /** @type {any} */ (phase))
      stage = 'idle'
      await settleFocus()
    }
  }

  function cancelConsent() {
    const current = proposal
    attempt += 1
    proposal = null
    stage = 'idle'
    if (current?.proposalId) discard(current.proposalId)
    void settleFocus()
  }

  /** @param {{ rightsAttested: boolean, retentionAcknowledged: boolean }} answers */
  async function confirm({ rightsAttested, retentionAcknowledged }) {
    if (stage !== 'consent' || !proposal) return
    const run = attempt
    const current = proposal
    const backend = getBackend()
    progress = { done: 0, total: (current.tiles?.length ?? 0) * (current.models?.length || 1) }
    stage = 'running'
    await tick()
    actions?.querySelector('[data-role="cloud-cancel"]')?.focus()
    try {
      const analysis = await backend.confirmRemoteAnalysis({ proposalId: current.proposalId, rightsAttested, retentionAcknowledged })
      if (run !== attempt || destroyed) return
      end()
      onresult(analysis, {
        provider: current.provider,
        profileName: current.profileName || current.profileId,
        capability: capabilityOf(current),
      })
    } catch (cause) {
      if (run !== attempt || destroyed) return
      const record = await statusOf(current.proposalId)
      if (run !== attempt || destroyed) return
      outcome = outcomeOfRun(cause, record ?? lastRecord)
      end()
    }
    await settleFocus()
  }

  /**
   * What a proposal sends to, as one id: `a+b` when a companion goes too.
   *
   * @param {any} current
   */
  function capabilityOf(current) {
    return Array.isArray(current?.models) && current.models.length > 1
      ? current.models.map((model) => model.capability).join('+')
      : current?.capability
  }

  /** @param {string} proposalId */
  async function statusOf(proposalId) {
    try { return await getBackend().getRemoteAnalysisStatus({ proposalId }) } catch { return null }
  }

  /**
   * The journal knows what the thrown text cannot: whether the run was
   * cancelled, or left a tile out with no answer. A failure keeps the thrown
   * text, which carries the code and its detail.
   *
   * @param {unknown} cause
   * @param {any} record
   */
  function outcomeOfRun(cause, record) {
    const phase = record?.phase?.phase
    if (phase === 'cancelled' || phase === 'unknown' || phase === 'unknown_remote_state') {
      const named = outcomeOfRecord(record)
      if (named?.kind === 'remoteUnknown') {
        const detail = typeof cause === 'string' ? cause : String(/** @type {any} */ (cause)?.message ?? '')
        return detail ? { ...named, detail } : named
      }
      if (named) return named
    }
    return outcomeOf(cause, 'confirm')
  }

  function end() {
    proposal = null
    progress = null
    lastRecord = null
    stage = 'idle'
  }

  async function cancelRun() {
    if (stage !== 'running' || !proposal) return
    const { proposalId } = proposal
    stage = 'cancelling'
    // The run's own rejection names the outcome. A cancel that arrives after
    // the last tile answers false, and the result simply lands.
    try { await getBackend().cancelRemoteAnalysis({ proposalId }) } catch { /* see above */ }
  }
</script>

{#if longstrip && (usable || unavailableKey)}
  <p class="unavailable" data-reason="longstrip">
    <Icon name="cloud" size={12} />
    <span>{t('cloud.analysis.unavailable.longstrip')}</span>
  </p>
{:else if usable && readiness.ready}
  <section class="cloud" aria-labelledby="{uid}-title" data-stage={stage}>
    {#if stage === 'consent' && proposal}
      <CloudAnalysisConsent
        {proposal}
        {pageNumber}
        capabilityKey={capabilityKeyOf(capabilityOf(proposal))}
        keyOf={capabilityKeyOf}
        providerKey={providerKeyOf(proposal.provider)}
        onconfirm={confirm}
        oncancel={cancelConsent}
      />
    {:else}
      <div class="strip">
        <div class="where">
          <span class="mark" aria-hidden="true"><Icon name="cloud" size={13} /></span>
          <div class="where-text">
            <h4 id="{uid}-title">{t('cloud.analysis.title')}</h4>
            <p class="target">{t('cloud.analysis.entry.target', { name: profileName, providerKey })}</p>
          </div>
        </div>
        {#if request}
          <p class="field chosen">
            <span>{t('cloud.analysis.capability.label')}</span>
            <strong>{t(capabilityKeyOf(request.companion ? `${request.capability}+${request.companion}` : request.capability))}</strong>
          </p>
        {:else}
          <label class="field">
            <span>{t('cloud.analysis.capability.label')}</span>
            <select bind:value={capability} disabled={active || locked}>
              {#each CLOUD_CAPABILITIES as option (option.id)}
                <option value={option.id}>
                  {unoffered.has(option.id) ? t('cloud.analysis.capability.notOffered', { name: t(option.key) }) : t(option.key)}
                </option>
              {/each}
            </select>
          </label>
        {/if}
        <span class="actions" bind:this={actions}>
          {#if stage === 'running' || stage === 'cancelling'}
            <Button data-role="cloud-cancel" disabled={stage === 'cancelling'} onclick={cancelRun}>
              <Icon name="stop" size={12} />{t('cloud.analysis.cancel')}
            </Button>
          {:else if !request}
            <Button data-role="cloud-start" disabled={active || locked} onclick={begin}>
              <Icon name="cloud" size={12} />{t('cloud.analysis.entry.action')}
            </Button>
          {/if}
        </span>
      </div>

      {#if progress && (stage === 'running' || stage === 'cancelling')}
        <div
          class="progress"
          role="progressbar"
          aria-label={t('cloud.analysis.progressLabel')}
          aria-valuemin="0"
          aria-valuemax={progress.total}
          aria-valuenow={progress.done}
          aria-valuetext={t('cloud.analysis.status.progress', { done: progress.done, total: progress.total, name: profileName })}
        ><span style:transform="scaleX({percentDone / 100})"></span></div>
      {/if}

      <div class="status" role="status">
        {#if stage === 'checking'}
          <p class="line working">{t('cloud.analysis.status.checking', { name: profileName })}</p>
        {:else if stage === 'proposing'}
          <p class="line working">{t('cloud.analysis.status.preparing')}</p>
        {:else if stage === 'running' && progress}
          <p class="line working">{t('cloud.analysis.status.progress', { done: progress.done, total: progress.total, name: profileName })}</p>
        {:else if stage === 'cancelling'}
          <p class="line working">{t('cloud.analysis.status.cancelling')}</p>
        {:else if outcome}
          <WorkflowOutcome {outcome} />
        {:else if request}
          <p class="line">{t('cloud.analysis.entry.routed', { name: profileName })}</p>
        {:else}
          <p class="line">{t('cloud.analysis.entry.note')}</p>
        {/if}
      </div>
    {/if}
  </section>
{:else if unavailableKey}
  <!-- The same strip, disabled: what is missing, and the way to fix it. -->
  <section class="cloud" aria-labelledby="{uid}-title" data-stage="unavailable">
    <div class="strip off">
      <div class="where">
        <span class="mark" aria-hidden="true"><Icon name="cloud" size={13} /></span>
        <div class="where-text">
          <h4 id="{uid}-title">{t('cloud.analysis.title')}</h4>
          <p class="target" data-reason={readiness.reason ?? 'off'}>{t(unavailableKey)}</p>
        </div>
      </div>
      <span class="actions">
        <Button data-role="cloud-settings" size="sm" variant="soft" onclick={openCloudSettings}>
          {t('cloud.analysis.unavailable.settings')}
        </Button>
      </span>
    </div>
  </section>
{/if}

<style>
  .cloud {
    display: grid;
    gap: var(--s-3);
    min-width: 0;
  }
  .strip {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: var(--s-3) var(--s-4);
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--panel);
  }
  .where { display: flex; align-items: center; gap: 8px; min-width: 0; margin-right: auto }
  .mark {
    display: inline-grid;
    place-items: center;
    flex: none;
    width: 26px;
    height: 26px;
    border-radius: var(--r-sm);
    background: var(--accent-soft);
    color: var(--t2);
  }
  .where-text { display: grid; gap: 1px; min-width: 0 }
  h4 { margin: 0; font-size: 12px; font-weight: 600; color: var(--text) }
  .target { margin: 0; font-size: 11.5px; color: var(--t2); overflow-wrap: anywhere }
  .field { display: grid; gap: 3px; min-width: 0 }
  .field > span { font-size: 11px; color: var(--t3) }
  .chosen { margin: 0 }
  .chosen > strong { font-size: 12px; font-weight: 500; color: var(--text); line-height: 26px }
  .strip.off .mark { background: var(--line); color: var(--t3) }
  select {
    height: 26px;
    min-width: 0;
    max-width: 100%;
    padding: 0 8px;
    border: 1px solid var(--line2);
    border-radius: var(--r-sm);
    background: var(--surface);
    color: var(--text);
    font-size: 12px;
  }
  select:disabled { opacity: .5 }
  .actions { display: inline-flex; align-items: center; gap: var(--s-2) }

  .progress {
    height: 3px;
    overflow: hidden;
    border-radius: 2px;
    background: var(--line);
  }
  .progress > span {
    display: block;
    height: 100%;
    background: var(--accent);
    transform-origin: left center;
    transition: transform .24s cubic-bezier(.22, 1, .36, 1);
  }
  @media (prefers-reduced-motion: reduce) {
    .progress > span { transition: none }
  }

  .status { display: grid; gap: var(--s-2) }
  .status:empty { display: none }
  .line { margin: 0; font-size: 11.5px; color: var(--t3); line-height: 1.45 }
  .line.working { color: var(--t2); font-variant-numeric: tabular-nums }

  .unavailable {
    display: flex;
    align-items: baseline;
    gap: 6px;
    margin: 0;
    font-size: 11.5px;
    color: var(--t3);
    line-height: 1.45;
  }
  .unavailable :global(svg) { flex: none; transform: translateY(2px) }
</style>
