<script>
  /**
   * Denoise a chapter's pages, from its menu on Home.
   *
   * One dialog, several views, in the order a run needs them:
   *
   * - **setup**, when denoise is off: pick this computer or the cloud GPU.
   *   Cloud is offered only while the deployment has page denoise
   *   (`cloudOffered`), asked when the dialog opens.
   *   The choice is saved as the setting, then the form follows.
   * - **form**: where to run and the preset (both start at the settings and
   *   can be changed here for this run), the estimated time for the whole
   *   chapter, and the folder the pages go to.
   * - **consent**, cloud only: what `prepare_cloud_denoise` says would be
   *   sent, where, and at what GPU time and cost, with the same two
   *   statements every cloud consent asks for (`CloudCleanConsentDialog`).
   *   Nothing is sent until Confirm; Back or a close discards the proposal.
   * - **running**: a bar filled by `denoise://progress`, which a cloud run
   *   now sends too (page by page), and a Stop that ends the run: a local
   *   one at its next tile, a cloud one after the pages already sent. The
   *   pages already saved stay.
   * - **summary**: pages saved, pages that failed and why, and the folder.
   *
   * Closing while it runs is allowed: the run belongs to the backend and
   * carries on. It is the jobs list's, not this dialog's
   * (`state/jobs.svelte.js#trackDenoise`), so its progress is read from there
   * and it shows under Jobs once the dialog is gone. Opened again for a
   * chapter whose denoise is still going, the dialog shows that run with its
   * Stop instead of a new form. While the dialog is up it says how the run
   * ended; closed, the jobs list says it as a notice.
   */
  import { onDestroy, onMount, untrack } from 'svelte'
  import { Button, Modal, Segmented, Select } from '../ui/index.js'
  import Icon from '../icons/Icon.svelte'
  import SourceFolderField from '../home/dialogs/SourceFolderField.svelte'
  import { closeModal } from '../state/app.svelte.js'
  import { DENOISE_KINDS, jobById, jobFraction, runningJobFor, stopJob, trackDenoise, watchJob } from '../state/jobs.svelte.js'
  import { projectById } from '../home/library.svelte.js'
  import { loadLibrary } from '../home/library.svelte.js'
  import { getBackend } from '../api/backend.js'
  import { t } from '../i18n/index.js'
  import { session } from '../state/session.svelte.js'
  import { cloud, cloudEntries, pickCloudChoice } from '../state/cloud.svelte.js'
  import { configurationKey, configurationFailed, rememberedConfiguration } from '../state/cloudconfig.svelte.js'
  import {
    checkCloudDenoise, cloudNote, cloudOffered, currentPreset, loadDenoisePresets, offeredPresets, saveDenoiseChoice,
  } from '../state/denoise.svelte.js'
  import { DENOISE_REFERENCE, chapterEstimate, defaultOutDir, pageMegapixels, presetById } from '../model/denoise.js'
  import { providerKeyOf } from './CloudAnalysis.svelte'
  import { openSettings } from './settingslinks.js'
  import PresetList from './denoise/PresetList.svelte'
  import LocalTime from './denoise/LocalTime.svelte'
  import ModelStatus from './denoise/ModelStatus.svelte'
  import Spinner from './denoise/Spinner.svelte'
  import { durationWords, presetTime } from './denoise/time.js'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()
  const uid = $props.id()

  const chapter = untrack(() => /** @type {any} */ (spec?.props?.chapter ?? null))
  /** Opened as Denoise cleaned chapter: its own title and folder, the same run. */
  const cleaned = untrack(() => spec?.props?.cleaned === true)
  const pages = Array.isArray(chapter?.pages) ? chapter.pages : []
  const count = pages.length

  /** A denoise this chapter already has going, picked up rather than started again. */
  const ongoing = untrack(() => runningJobFor(chapter?.id, DENOISE_KINDS))

  /** @typedef {'setup'|'form'|'preparing'|'consent'|'running'|'summary'|'error'} Phase */
  /** @type {Phase} */
  let phase = $state(untrack(() => (ongoing ? 'running' : session.denoiseTarget === 'off' ? 'setup' : 'form')))
  /** Where this run goes; starts at the setting, and Cloud only while it can. */
  let target = $state(untrack(() => (ongoing ? (ongoing.kind === 'cloudDenoise' ? 'cloud' : 'local') : startTarget())))
  let preset = $state(untrack(() => currentPreset(target)))
  let outDir = $state(defaultOutDir(chapter?.sourcePath, cleaned ? 'denoised-cleaned' : 'denoised'))
  let installed = $state(false)
  let setupFailed = $state(false)
  /** @type {any} */
  let proposal = $state(null)
  let rights = $state(false)
  let retention = $state(false)
  /** @type {import('../api/backend.js').DenoiseReport|null} */
  let report = $state(null)
  /** @type {{key: string, params?: Record<string, unknown>}|null} */
  let failure = $state(null)
  let copied = $state(false)
  /** The run this dialog shows, local or cloud. Its progress is the jobs list's. */
  let runId = $state(ongoing?.runId ?? '')
  const job = $derived(runId ? jobById(runId) : null)
  const stopping = $derived(job?.stopping === true)
  /** @type {(() => void)|null} */
  let unwatch = ongoing ? watchJob(ongoing.runId) : null
  let open = true

  onMount(() => {
    loadDenoisePresets()
    checkCloudDenoise()
  })
  onDestroy(() => {
    open = false
    unwatch?.()
    // A proposal left on screen is discarded with the dialog: an open
    // proposal is a consent nobody is looking at.
    if (phase === 'consent' && proposal?.proposalId) discard()
  })

  // The run ended while the dialog is up: say how, here.
  $effect(() => {
    if (phase !== 'running' || !job || job.status === 'running') return
    untrack(() => settle(job))
  })

  function startTarget() {
    const stored = session.denoiseTarget
    if (stored === 'cloud' && !cloudOffered()) return 'local'
    return stored === 'off' ? 'local' : stored
  }

  // The deployment answered after the dialog opened: a Cloud it cannot run
  // falls back to this computer before anything is sent.
  $effect(() => {
    if (phase === 'form' && target === 'cloud' && !cloudOffered()) untrack(() => chooseTarget('local'))
  })

  const presets = $derived(offeredPresets(target))
  const chosen = $derived(presets.some((entry) => entry.id === preset) ? preset : presets[0]?.id ?? null)
  const estimate = $derived(chapterEstimate({
    target,
    preset: chosen ? presetById(chosen) : null,
    pages,
    localPerPage: session.denoiseLocalSecondsPerPage,
  }))
  const sized = $derived(pageMegapixels(pages) !== null)

  const targets = $derived([
    { value: 'local', label: t('denoise.target.local') },
    { value: 'cloud', label: t('denoise.target.cloud'), disabled: !cloudOffered() },
  ])
  const note = $derived(cloudNote())

  /**
   * Every saved cloud profile, once there is more than one: picking another
   * makes it the default, as an engine picker does, and its deployment is
   * asked about page denoise again. A run already going on the old one goes on.
   */
  const profiles = $derived(cloudEntries()?.map(({ value, choice }) => ({
    value, label: choice?.name ?? t('denoise.target.cloud'),
    disabled: rememberedConfiguration(configurationKey(cloud.readiness, choice))?.denoise?.state === 'missing',
  })) ?? null)
  let switching = $state(false)

  /** @param {string} value */
  async function pickProfile(value) {
    switching = true
    try {
      if (await pickCloudChoice(value)) await checkCloudDenoise()
    } finally {
      switching = false
    }
    preset = currentPreset(target)
  }

  const folder = $derived(outDir.trim())
  const canRun = $derived(Boolean(chosen && folder && count > 0 &&
    (target === 'local' ? installed : cloudOffered() && !switching)))

  /** @param {string} next */
  function chooseTarget(next) {
    target = next
    preset = currentPreset(next)
  }

  /** @param {'local'|'cloud'} next */
  async function setUp(next) {
    setupFailed = !(await saveDenoiseChoice({ target: next }))
    if (setupFailed) return
    chooseTarget(next)
    phase = 'form'
  }

  /** The code a rejection carries, without the `Error: ` a thrown one adds. @param {unknown} error */
  function codeOf(error) {
    const text = error instanceof Error ? error.message : String(error ?? '')
    return text.replace(/^Error:\s*/, '').trim() || 'unknown'
  }

  /** @param {string} code */
  function errorOf(code) {
    if (code.startsWith('cloud_disabled')) return { key: 'denoise.error.cloudDisabled' }
    if (code.startsWith('cloud_denoise_profile') || code.startsWith('credential_missing')) return { key: 'denoise.error.noProfile' }
    // The deployment has no denoise route or models: it was set up with Page
    // denoise off, or on Beam, which cannot run it.
    if (code.startsWith('capability_unavailable: gateway denoise is not configured') ||
        code.startsWith('capability_unavailable: the deployment has no models')) return { key: 'denoise.error.notSetUp' }
    if (code.startsWith('denoise_models_missing')) return { key: 'denoise.error.modelMissing' }
    if (code.startsWith('denoise_runtime_unavailable')) return { key: 'denoise.error.runtimeMissing' }
    if (code.includes('out_dir')) return { key: 'denoise.error.outDir' }
    return { key: 'denoise.error.unknown', params: { code } }
  }

  /**
   * A failed page's reason, in words. Local codes are the cloud ones without
   * the `cloud_` prefix. @param {string} code
   */
  function reasonOf(code) {
    const bare = code.replace(/^cloud_/, '')
    if (bare.startsWith('denoise_page_too_large')) return t('denoise.reason.tooLarge')
    if (bare.startsWith('denoise_unsupported_page')) return t('denoise.reason.unsupported')
    if (bare.startsWith('denoise_inference_failed')) return t('denoise.reason.inference')
    if (bare.startsWith('denoise_page_unreadable') || bare.startsWith('denoise_page_missing')) return t('denoise.reason.unreadable')
    if (bare.startsWith('denoise_write_failed') || bare.startsWith('denoise_duplicate_output')) return t('denoise.reason.writeFailed')
    if (bare.startsWith('denoise_page_changed')) return t('denoise.reason.changed')
    if (code.startsWith('capability_unavailable')) return t('denoise.reason.unavailable')
    return t('denoise.reason.unknown', { code })
  }

  /** The caller's name for a run, local or cloud: `denoise://progress` carries it. */
  const mintRunId = () => `den-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`

  async function run() {
    if (!canRun || !chosen) return
    if (target === 'cloud') return review()
    const backend = getBackend()
    const id = mintRunId()
    const presetId = chosen
    await follow(id, 'denoise', () => backend.denoiseChapterLocal({ runId: id, chapterId: chapter.id, pageIndices: null, presetId, outDir: folder }))
  }

  /**
   * Hand the run to the jobs list and show it. The list outlives the dialog;
   * the dialog only watches, so while it is up the ending is said here.
   *
   * @param {string} id
   * @param {'denoise'|'cloudDenoise'} kind
   * @param {() => Promise<import('../api/backend.js').DenoiseReport>} start
   */
  async function follow(id, kind, start, target = null) {
    unwatch?.()
    unwatch = watchJob(id)
    runId = id
    phase = 'running'
    const project = projectById(untrack(() => spec?.props?.projectId ?? null))
    await trackDenoise({
      runId: id,
      kind,
      chapterId: chapter.id,
      projectId: project?.id ?? /** @type {string|null} */ (untrack(() => spec?.props?.projectId ?? null)),
      projectName: project?.name ?? null,
      chapterName: chapter?.name ?? null,
      chapterNumber: chapter?.number ?? null,
      total: count,
      target,
    }, async () => {
      const answer = await start()
      // The chapter list reads the pages written: a finished denoise can put
      // "Replace pages with denoised" on the chapter's menu. Said whether or
      // not the dialog is still up.
      if (Array.isArray(answer?.written) && answer.written.length) loadLibrary()
      return answer
    })
  }

  /** @param {import('../state/jobs.svelte.js').Job} ended */
  function settle(ended) {
    if (!open) return
    if (ended.status === 'failed') {
      failure = errorOf(ended.errorCode ?? 'unknown')
      phase = 'error'
      return
    }
    report = ended.report ?? { written: [], failed: [], cancelled: ended.status === 'cancelled' }
    phase = 'summary'
  }

  function stopRun() {
    if (!runId || stopping) return
    stopJob(runId)
  }

  /** How far the run is, 0 to 1. */
  const fraction = $derived(job ? jobFraction(job) : 0)
  /** The progress line's figures, from the jobs list, once the run has said how far it is. */
  const progress = $derived(job && job.total > 0 && (job.done > 0 || job.page > 0) ? { done: job.done, total: job.total } : null)

  async function review() {
    const entry = chosen ? presetById(chosen) : null
    if (!entry) return
    phase = 'preparing'
    const profileId = cloud.readiness.target?.profile_id
    const configKey = configurationKey(cloud.readiness)
    try {
      const prepared = await getBackend().prepareCloudDenoise({ chapterId: chapter.id, pageIndices: null, recipe: entry.recipe })
      proposal = { ...prepared, profileId: prepared.profileId ?? profileId }
      rights = false
      retention = false
      if (open) phase = 'consent'
      else discard()
    } catch (error) {
      await configurationFailed(getBackend(), configKey, 'denoise', error)
      failure = errorOf(codeOf(error))
      phase = 'error'
    }
  }

  async function confirm() {
    if (!proposal) return
    const standing = proposal.standing === true
    if (!standing && (!rights || !retention)) return
    const held = proposal
    phase = 'running'
    let grant
    try {
      grant = await getBackend().confirmCloudDenoise({
        proposalId: held.proposalId,
        planDigest: held.planDigest,
        rightsAttested: standing ? false : rights,
        retentionAcknowledged: standing ? false : retention,
      })
    } catch (error) {
      proposal = null
      failure = errorOf(codeOf(error))
      phase = 'error'
      return
    }
    proposal = null
    const id = mintRunId()
    const backend = getBackend()
    await follow(id, 'cloudDenoise', () => backend.startCloudDenoise({ grantId: grant.grantId, outDir: folder, runId: id }), {
      provider: held.provider, profileId: held.profileId,
    })
  }

  function discard() {
    const id = proposal?.proposalId
    proposal = null
    if (id) getBackend().cancelCloudDenoise({ proposalId: id }).catch(() => {})
  }

  function back() {
    if (phase === 'consent') discard()
    failure = null
    phase = 'form'
  }

  function close() {
    if (phase === 'consent') discard()
    closeModal(phase === 'summary' ? 'done' : null)
  }

  async function copyPath() {
    try {
      await globalThis.navigator?.clipboard?.writeText(folder)
      copied = true
    } catch {
      copied = false
    }
  }

  // The consent's figures, fixed by the proposal.
  const numbers = new Intl.NumberFormat()
  const consent = $derived.by(() => {
    if (!proposal) return null
    const seconds = proposal.estimatedGpuSeconds
    const range = proposal.estimatedCostUsd
    return {
      pages: Number(proposal.pages) || 0,
      pixels: numbers.format(proposal.totalPixels ?? 0),
      tooLarge: Array.isArray(proposal.tooLargePages) ? proposal.tooLargePages.length : 0,
      models: Array.isArray(proposal.modelIds) ? proposal.modelIds : [],
      minutes: seconds && Number.isFinite(seconds.low) && Number.isFinite(seconds.high) && seconds.high >= seconds.low
        ? { low: Math.max(1, Math.round(seconds.low / 60)), high: Math.max(1, Math.round(seconds.high / 60)) }
        : null,
      cost: range && Number.isFinite(range.low) && Number.isFinite(range.high) && range.low >= 0 && range.high >= range.low
        ? { low: range.low, high: range.high }
        : null,
      expires: Number.isFinite(proposal.expiresAtMs) && proposal.expiresAtMs > 0
        ? new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }).format(proposal.expiresAtMs)
        : null,
      standing: proposal.standing === true,
    }
  })
  const confirmable = $derived(Boolean(consent && (consent.standing || (rights && retention))))

  const title = $derived(phase === 'consent' ? t('denoise.consent.title') : t(cleaned ? 'denoise.titleCleaned' : 'denoise.title'))
</script>

<Modal
  {title}
  meta={t('denoise.meta', { count, number: chapter?.number ?? '' })}
  width={560}
  blocking={phase === 'consent'}
  onclose={close}
>
  <div class="denoise" data-phase={phase}>
    {#if phase === 'setup'}
      <p class="lead">{t('denoise.setup.heading')}</p>
      <p class="quiet">{t('denoise.setup.body')}</p>
      <div class="choices" role="group" aria-label={t('denoise.target.label')}>
        <button type="button" class="choice" data-target="local" onclick={() => setUp('local')}>
          <span class="name">{t('denoise.target.local')}</span>
          <span class="about">{t('onboarding.denoise.localNote')}</span>
        </button>
        <button
          type="button"
          class="choice"
          data-target="cloud"
          disabled={!cloudOffered()}
          aria-describedby={note ? `${uid}-cloud-why` : undefined}
          onclick={() => setUp('cloud')}
        >
          <span class="name">{t('denoise.target.cloud')}</span>
          <span class="about">{t('onboarding.denoise.cloudNote')}</span>
        </button>
      </div>
      {#if note}<p class="small" id="{uid}-cloud-why">{t(note)}</p>{/if}
      {#if setupFailed}<p class="small failed" role="alert">{t('onboarding.saveFailed')}</p>{/if}
    {:else if phase === 'form'}
      <div class="row">
        <span class="label" id="{uid}-where">{t('denoise.target.label')}</span>
        <Segmented
          options={targets}
          value={target}
          labelledBy="{uid}-where"
          describedBy={note ? `${uid}-cloud-why` : undefined}
          onchange={chooseTarget}
        />
      </div>
      {#if profiles}
        <div class="row">
          <span class="label" id="{uid}-profile">{t('denoise.target.profile')}</span>
          <Select options={profiles} value="cloud" labelledBy="{uid}-profile" disabled={switching} onchange={pickProfile} />
        </div>
      {/if}
      {#if note}<p class="small" id="{uid}-cloud-why">{t(note)}</p>{/if}

      <div class="section">
        <span class="label">{t('denoise.preset.label')}</span>
        <PresetList
          {presets}
          value={chosen}
          label={t('denoise.preset.label')}
          timeOf={(entry) => presetTime(entry, target, { pages: sized ? pages : null, localPerPage: session.denoiseLocalSecondsPerPage })}
          onchange={(id) => (preset = id)}
        />
      </div>

      {#if target === 'local'}
        <div class="section" class:hidden={installed}>
          <span class="label">{t('settings.denoise.localModel')}</span>
          <ModelStatus bind:installed />
        </div>
      {/if}

      <div class="section estimate" data-basis={estimate.basis}>
        <span class="label">{t('denoise.estimate.label')}</span>
        {#if estimate.seconds !== null}
          <p class="figure">{t('denoise.estimate.value', { duration: durationWords(estimate.seconds) })}</p>
          <p class="small">
            {#if estimate.basis === 'pages'}{t('denoise.estimate.pages')}
            {:else if estimate.basis === 'reference'}{t('denoise.estimate.reference', DENOISE_REFERENCE)}
            {:else}{t('denoise.estimate.measured')}{/if}
          </p>
        {:else}
          <LocalTime {installed} />
        {/if}
      </div>

      <div class="section">
        <SourceFolderField
          value={outDir}
          onchange={(value) => (outDir = value)}
          label={t('denoise.out.label')}
          placeholder={t('denoise.out.placeholder')}
          browseLabel={t('shell.action.chooseFolder')}
          chooserTitle={t('denoise.out.chooserTitle')}
        />
        <p class="small">{t('denoise.out.note')}</p>
      </div>
    {:else if phase === 'running' && runId}
      <div class="run">
        <div
          class="progress"
          role="progressbar"
          aria-label={t('denoise.busy.progressLabel')}
          aria-valuemin="0"
          aria-valuemax="100"
          aria-valuenow={Math.round(fraction * 100)}
        ><span style:transform="scaleX({fraction})"></span></div>
        <p class="line" role="status">
          {#if stopping}{t('denoise.busy.stopping')}
          {:else if progress && progress.done < progress.total}{t('denoise.busy.page', { page: progress.done + 1, total: progress.total })}
          {:else}{t('denoise.busy.starting')}{/if}
        </p>
        <p class="small">{t(target === 'cloud' ? 'denoise.busy.cloud' : 'denoise.busy.local')}</p>
        <p class="small">{t('denoise.busy.background')}</p>
      </div>
    {:else if phase === 'preparing' || phase === 'running'}
      <div class="busy" role="status">
        <Spinner size={18} />
        <p>
          {#if phase === 'preparing'}{t('denoise.busy.preparing')}
          {:else if target === 'cloud'}{t('denoise.busy.cloud')}
          {:else}{t('denoise.busy.local')}{/if}
        </p>
      </div>
    {:else if phase === 'consent' && consent}
      <p class="heading"><Icon name="cloud" size={14} />{t('denoise.consent.heading', { count: consent.pages })}</p>
      <dl class="facts">
        <div class="fact">
          <dt>{t('cloud.analysis.run.what')}</dt>
          <dd data-fact="what">{t('denoise.consent.what', { count: consent.pages, pixels: consent.pixels })}</dd>
        </div>
        {#if consent.tooLarge > 0}
          <div class="fact">
            <dt>{t('denoise.consent.tooLarge')}</dt>
            <dd data-fact="tooLarge">{t('denoise.consent.tooLargeValue', { count: consent.tooLarge })}</dd>
          </div>
        {/if}
        <div class="fact">
          <dt>{t('denoise.consent.models')}</dt>
          <dd data-fact="models">{#each consent.models as model (model)}<code>{model}</code>{/each}</dd>
        </div>
        <div class="fact">
          <dt>{t('cloud.analysis.run.where')}</dt>
          <dd data-fact="where">{t('cloud.analysis.consent.whereValue', { name: proposal.profileName, providerKey: providerKeyOf(proposal.provider) })}</dd>
        </div>
        <div class="fact">
          <dt>{t('cloud.clean.gpu')}</dt>
          <dd data-fact="gpu">{proposal.gpu || t('cloud.clean.gpuUnknown')}</dd>
        </div>
        <div class="fact">
          <dt>{t('cloud.analysis.consent.cost')}</dt>
          <dd>
            <span data-cost={consent.cost ? 'estimate' : 'unknown'}>{consent.cost ? t('cloud.clean.costRange', consent.cost) : t('cloud.analysis.costUnknown')}</span>
            {#if consent.minutes}<span class="quiet" data-fact="gpu-time">{t('cloud.clean.gpuTime', consent.minutes)}</span>{/if}
            <span class="quiet">{t('denoise.consent.costBasis')}</span>
          </dd>
        </div>
        <div class="fact">
          <dt>{t('cloud.analysis.run.result')}</dt>
          <dd>
            <span>{t('denoise.consent.result', { path: folder })}</span>
            {#if proposal.planDigest}
              <details class="identity">
                <summary>{t('cloud.clean.plan')}</summary>
                <code data-fact="plan">{proposal.planDigest}</code>
              </details>
            {/if}
          </dd>
        </div>
      </dl>
      {#if consent.standing}
        <p class="project" data-fact="standing">{t('denoise.consent.standing')}</p>
      {:else}
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
        <p class="project">{t('denoise.consent.project')}</p>
      {/if}
      {#if consent.expires}<p class="expires">{t('cloud.analysis.consent.expires', { time: consent.expires })}</p>{/if}
    {:else if phase === 'summary' && report}
      <p class="result" data-written={report.written.length}>
        <Icon name={report.failed.length ? 'warning-triangle' : 'check'} size={14} />
        {t('denoise.summary.written', { count: report.written.length })}
      </p>
      {#if report.cancelled}<p class="small" data-stopped>{t('denoise.summary.stopped')}</p>{/if}
      {#if report.failed.length}
        <p class="failed-count">{t('denoise.summary.failed', { count: report.failed.length })}</p>
        <ul class="failures">
          {#each report.failed as page (page.pageIndex)}
            <li data-page={page.pageIndex}>
              <span class="page">{t('denoise.summary.page', { page: page.pageIndex + 1 })}</span>
              <span class="quiet">{reasonOf(page.code)}</span>
            </li>
          {/each}
        </ul>
      {/if}
      <div class="where">
        <code class="path">{folder}</code>
        <Button size="sm" onclick={copyPath}>{t('denoise.action.copyPath')}</Button>
        {#if copied}<span class="small" role="status">{t('denoise.summary.copied')}</span>{/if}
      </div>
    {:else if phase === 'error' && failure}
      <p class="failed-line" role="alert">{t(failure.key, failure.params ?? {})}</p>
      {#if failure.key === 'denoise.error.runtimeMissing'}
        <Button size="sm" onclick={() => { closeModal(null); openSettings('models') }}>{t('denoise.model.openSettings')}</Button>
      {:else if failure.key === 'denoise.error.notSetUp'}
        <Button size="sm" onclick={() => { closeModal(null); openSettings('cloud') }}>{t('denoise.error.openCloud')}</Button>
      {/if}
    {/if}
  </div>

  {#snippet buttons()}
    {#if phase === 'setup'}
      <Button onclick={close}>{t('shell.action.cancel')}</Button>
    {:else if phase === 'form'}
      <Button onclick={close}>{t('shell.action.cancel')}</Button>
      <Button variant="primary" disabled={!canRun} onclick={run}>
        {#if target === 'cloud'}<Icon name="cloud" size={12} />{t('denoise.action.review')}
        {:else}{t('denoise.action.run', { count })}{/if}
      </Button>
    {:else if phase === 'running' && runId}
      <Button onclick={close}>{t('shell.action.close')}</Button>
      <Button aria-disabled={stopping} onclick={stopRun}><Icon name="stop" size={12} />{t('denoise.action.stop')}</Button>
    {:else if phase === 'preparing' || phase === 'running'}
      <Button onclick={close}>{t('shell.action.close')}</Button>
    {:else if phase === 'consent'}
      <Button onclick={back}>{t('denoise.action.back')}</Button>
      <Button variant="primary" disabled={!confirmable} onclick={confirm}>
        <Icon name="cloud" size={12} />{t('shell.action.confirmSpend')}
      </Button>
    {:else if phase === 'error'}
      <Button onclick={close}>{t('shell.action.close')}</Button>
      <Button variant="primary" onclick={back}>{t('denoise.action.retry')}</Button>
    {:else}
      <Button variant="primary" onclick={close}>{t('shell.action.done')}</Button>
    {/if}
  {/snippet}
</Modal>

<style>
  .denoise { display: grid; gap: var(--s-5); margin-top: var(--s-4) }

  .lead { margin: 0; font-size: 13px; font-weight: 600; color: var(--text) }
  .quiet { color: var(--t2) }
  p.quiet { margin: calc(-1 * var(--s-3)) 0 0; font-size: 12px; line-height: 1.5 }
  .small { margin: 0; font-size: 11px; line-height: 1.45; color: var(--t3); max-width: 68ch }
  .small.failed, .failed-line { color: var(--warn) }
  .failed-line { margin: 0; font-size: 12px; line-height: 1.5 }

  .row { display: flex; align-items: center; justify-content: space-between; gap: var(--s-5) }
  .label { font-size: 11px; color: var(--t3) }
  .section { display: grid; gap: var(--s-3) }
  .section.hidden { display: none }
  .figure { margin: 0; font-size: 15px; font-weight: 600; font-variant-numeric: tabular-nums; color: var(--text) }
  .estimate { gap: var(--s-2) }

  .choices { display: grid; grid-template-columns: 1fr 1fr; gap: var(--s-4) }
  .choice {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    padding: var(--s-5);
    border: none;
    border-radius: var(--r-xl);
    background: var(--panel);
    box-shadow: 0 0 0 1px var(--line);
    color: var(--text);
    text-align: left;
    cursor: pointer;
    transition: box-shadow var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease);
  }
  .choice:hover:not(:disabled) { background: var(--accent-soft); box-shadow: 0 0 0 1px var(--line2) }
  .choice:active:not(:disabled) { transform: scale(.99) }
  .choice:disabled { cursor: default; opacity: .5 }
  .name { font-size: 13px; font-weight: 600 }
  .about { color: var(--t2); font-size: 12px; line-height: 1.5 }

  .busy { display: flex; align-items: center; gap: var(--s-4); padding: var(--s-6) 0 }

  /* A local run: the bar and line of `CloudAnalysis`'s run, so every run in
     the app reads the same. */
  .run { display: grid; gap: var(--s-3); padding: var(--s-4) 0 }
  .run .line { margin: 0; font-size: 12.5px; line-height: 1.5; color: var(--text); font-variant-numeric: tabular-nums }
  .progress { height: 3px; overflow: hidden; border-radius: 2px; background: var(--line) }
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
  .busy p { margin: 0; font-size: 12.5px; line-height: 1.5; color: var(--text) }

  /* The cloud consent's layout, fact for fact (`CloudCleanConsentDialog`),
     so every cloud question reads as one kind of question. */
  .heading { display: flex; align-items: center; gap: 6px; margin: 0; font-size: 12.5px; font-weight: 600; color: var(--text) }
  .facts { display: grid; gap: 8px; margin: 0 }
  .fact { display: grid; grid-template-columns: 112px minmax(0, 1fr); gap: 2px 12px; align-items: baseline }
  .fact > dt { font-size: 10.5px; font-weight: 600; letter-spacing: .04em; text-transform: uppercase; color: var(--t3) }
  .fact > dd { display: grid; gap: 2px; margin: 0; min-width: 0; font-size: 12px; line-height: 1.45; color: var(--text); overflow-wrap: anywhere }
  .identity { font-size: 11.5px; color: var(--t2) }
  .identity > summary { cursor: pointer; width: max-content }
  code { font-size: 11px; color: var(--t2); overflow-wrap: anywhere }
  .answers { display: grid; gap: 8px; padding-top: var(--s-4); border-top: 1px solid var(--line) }
  .answer { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px; align-items: start; font-size: 12px; line-height: 1.45; color: var(--text) }
  .answer input { margin: 2px 0 0; cursor: pointer; accent-color: var(--accent) }
  .answer label { cursor: pointer }
  .project { margin: 0; font-size: 12px; color: var(--t2) }
  .expires { margin: 0; font-size: 11px; color: var(--t3) }

  .result { display: flex; align-items: center; gap: var(--s-2); margin: 0; font-size: 14px; font-weight: 600; color: var(--text) }
  .failed-count { margin: 0; font-size: 12px; color: var(--warn) }
  .failures { display: grid; gap: 4px; margin: 0; padding: 0; list-style: none; max-height: 160px; overflow-y: auto }
  .failures li { display: flex; gap: var(--s-3); font-size: 12px; line-height: 1.45 }
  .page { flex: none; min-width: 64px; font-variant-numeric: tabular-nums; color: var(--text) }
  .where { display: flex; align-items: center; flex-wrap: wrap; gap: var(--s-3); padding-top: var(--s-4); border-top: 1px solid var(--line) }
  .path { flex: 1; min-width: 0; font-size: 11.5px; user-select: text }

  @media (max-width: 560px) {
    .choices { grid-template-columns: 1fr }
    .fact { grid-template-columns: minmax(0, 1fr) }
    .row { flex-direction: column; align-items: stretch; gap: var(--s-2) }
  }
</style>
