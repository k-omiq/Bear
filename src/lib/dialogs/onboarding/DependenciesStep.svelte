<script>
  /**
   * Setup step: what this computer needs for the choices so far, and what of
   * it is already here.
   *
   * Everything is read from the backend rather than guessed: the platform and
   * the runtime build from `listModels`, the graphics acceleration from the
   * runtime itself once it is installed (`runtimeReady`), and the FLUX helper
   * from the capabilities. Each platform gets its own runtime build, and some
   * builds need a toolkit installed by hand, which is said here.
   */
  import { t } from '../../i18n/index.js'
  import { Select } from '../../ui/index.js'
  import { writeSettingsSerialized } from '../../state/settingswrite.js'
  import { getBackend } from '../../api/backend.js'
  import { capabilities } from '../../state/capabilities.svelte.js'
  import { backendSettingsPatch, session, setModelAccelerator } from '../../state/session.svelte.js'
  import { CLEANERS, localDetectorModels, model, rescueRuns, runDetection } from '../../model/pipelines.js'
  import { RUNTIME_ID, runtimeReady } from '../firstlaunch.js'
  import {
    chosenBytes,
    chosenFiles,
    finishedFiles,
    firstLaunch,
    loadFirstLaunchAccelerators,
  } from '../firstlaunch.svelte.js'

  /** Each platform id's name, chosen between rather than built. */
  const PLATFORM_KEYS = {
    'macos-arm64': 'onboarding.dependencies.platform.macArm',
    'macos-x64': 'onboarding.dependencies.platform.macIntel',
    'windows-x64': 'onboarding.dependencies.platform.windows',
    'windows-arm64': 'onboarding.dependencies.platform.windowsArm',
    'linux-x64': 'onboarding.dependencies.platform.linux',
    'linux-arm64': 'onboarding.dependencies.platform.linuxArm',
  }

  /** Build names worth saying. `stock` is the plain build and says nothing. */
  const FLAVOUR_NAMES = { directml: 'DirectML', cuda12: 'CUDA 12', cuda13: 'CUDA 13' }

  const plan = $derived(firstLaunch.plan)
  const done = $derived(finishedFiles())
  const files = $derived(chosenFiles())
  const pending = $derived(files.filter((id) => !plan?.files[id]?.installed && !done[id]))
  const cleaningIds = $derived(new Set(CLEANERS.flatMap((engine) => engine.files)))
  const groups = $derived([
    { key: 'pipelines.detection', ids: files.filter((id) => id !== RUNTIME_ID && !cleaningIds.has(id)) },
    { key: 'pipelines.cleaning', ids: files.filter((id) => cleaningIds.has(id)) },
  ])
  const platformName = $derived(
    t(PLATFORM_KEYS[/** @type {keyof typeof PLATFORM_KEYS} */ (plan?.platform ?? '')] ?? 'onboarding.dependencies.platform.unknown'),
  )
  const build = $derived(
    [plan?.runtime.version, FLAVOUR_NAMES[/** @type {keyof typeof FLAVOUR_NAMES} */ (plan?.runtime.flavour ?? '')]]
      .filter(Boolean)
      .join(' · '),
  )
  const ready = $derived(runtimeReady(plan, done, firstLaunch.current))

  // Asked once the runtime is here, which can happen while this step is
  // open: a run started earlier may finish underneath it.
  let asked = false
  $effect(() => {
    if (!ready || asked || firstLaunch.accelerators) return
    asked = true
    loadFirstLaunchAccelerators()
  })

  const acceleration = $derived.by(() => {
    if (!ready) return t('onboarding.dependencies.afterRuntime')
    if (firstLaunch.acceleratorsFailed) return t('onboarding.dependencies.unreadable')
    if (!firstLaunch.accelerators) return '…'
    const names = firstLaunch.accelerators.providers
      .filter((provider) => provider.available && provider.id !== 'cpu')
      .map((provider) => t(provider.labelKey))
    return names.length ? names.join(', ') : t('onboarding.dependencies.cpuOnly')
  })

  let backendSaveFailed = $state(false)
  // The models that run on this computer: with Detect on set to Cloud GPU,
  // that run's own CTD rather than this computer's selection. The Hayai text
  // reader (`hayai`) runs here whenever the run reads, the cloud run included.
  const reads = $derived(rescueRuns(session.detection, runDetection(session).ocrRescue))
  const selectedModels = $derived(new Set([...localDetectorModels(session), ...(reads ? ['hayai'] : []), 'inpainter']))
  const modelRows = $derived((firstLaunch.accelerators?.models ?? []).filter((row) => selectedModels.has(row.id)))

  function modelName(row) {
    return row.modelName ?? model(row.id)?.product ?? (row.id === 'inpainter' ? CLEANERS[0].name : t(row.modelKey))
  }

  function backendChoices(row) {
    const choices = [
      { value: 'inherit', label: t('settings.accel.inherit', { backend: t(session.accelerator === 'auto' ? 'settings.accel.auto' : `accel.${session.accelerator}`) }) },
      { value: 'auto', label: t('settings.accel.auto') },
    ]
    for (const status of row.backendStatus ?? []) {
      const provider = firstLaunch.accelerators?.providers.find((entry) => entry.id === status.id)
      const name = provider ? t(provider.labelKey) : status.id
      const level = status.verified ? 'verified' : status.available ? 'available' : status.installed ? 'installed' : status.supported ? 'supported' : 'unsupported'
      choices.push({
        value: status.id,
        label: `${name} · ${status.reasonKey ? t(status.reasonKey) : t(`settings.accel.state.${level}`)}`,
        disabled: !status.supported || !status.available,
      })
    }
    return choices
  }

  async function chooseModelBackend(modelId, id) {
    const before = session.modelAccelerators[modelId] ?? 'inherit'
    backendSaveFailed = false
    setModelAccelerator(modelId, id)
    try {
      await writeSettingsSerialized(getBackend(), () => backendSettingsPatch())
      firstLaunch.accelerators = await getBackend().listAccelerators()
    } catch {
      setModelAccelerator(modelId, before)
      backendSaveFailed = true
    }
  }

  /** @param {string} id */
  function stateOf(id) {
    const row = plan?.files[id]
    if (!row || row.installed || done[id]) return t('onboarding.dependencies.installed')
    return t('onboarding.dependencies.toDownload', { bytes: row.bytes })
  }
</script>

<p class="lead">{t('onboarding.dependencies.body', { platform: platformName })}</p>

{#if modelRows.length}
  <h2 class="group">{t('settings.accel.models')}</h2>
  <p class="lead">{t('settings.accel.modelHelp')}</p>
  {#if backendSaveFailed}<p class="need" role="alert">{t('settings.accel.saveFailed')}</p>{/if}
  <div class="model-backends">
    {#each modelRows as row (row.id)}
      <div class="model-backend">
        <label for="onboarding-backend-{row.id}">{modelName(row)}</label>
        <Select id="onboarding-backend-{row.id}" label={t('settings.accel.modelLabel', { model: modelName(row) })}
          options={backendChoices(row)} value={session.modelAccelerators[row.id] ?? 'inherit'}
          onchange={(value) => chooseModelBackend(row.id, value)} />
        {#if row.id === 'samTs' || row.id === 'rtFull'}
          <span class="sub">{t('settings.accel.cloudReview')}</span>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<dl class="rows">
  <div class="row">
    <dt>{t('onboarding.dependencies.runtime')}{#if build}<span class="sub">{build}</span>{/if}</dt>
    <dd class:warn={plan?.runtimeUnavailable}>
      {plan?.runtimeUnavailable ? t('onboarding.dependencies.unavailable') : stateOf(RUNTIME_ID)}
      {#if plan?.runtime.needs.length}
        <span class="need">{t('onboarding.dependencies.needs', { items: plan.runtime.needs.join(', ') })}</span>
      {/if}
    </dd>
  </div>
  <div class="row">
    <dt>{t('onboarding.dependencies.acceleration')}</dt>
    <dd>{acceleration}</dd>
  </div>
  <div class="row">
    <dt>{t('onboarding.dependencies.helper')}</dt>
    <dd>{capabilities.sidecar ? t('onboarding.dependencies.helperFound') : t('onboarding.dependencies.helperMissing')}</dd>
  </div>
</dl>

{#each groups as group (group.key)}
  {#if group.ids.length}
    <h2 class="group">{t(group.key)}</h2>
    <dl class="rows">
      {#each group.ids as id (id)}
        <div class="row"><dt>{t(plan?.files[id]?.labelKey ?? '')}</dt><dd>{stateOf(id)}</dd></div>
      {/each}
    </dl>
  {/if}
{/each}

<p class="total">
  {pending.length
    ? t('onboarding.dependencies.total', { count: pending.length, bytes: chosenBytes() })
    : t('onboarding.dependencies.nothing')}
</p>

<style>
  .rows { margin: 0 0 var(--s-6) }
  .row {
    display: flex;
    justify-content: space-between;
    gap: var(--s-6);
    padding: var(--s-4) 0;
    border-top: 1px solid var(--line);
  }
  dt { font-weight: 500 }
  .sub { margin-left: var(--s-3); color: var(--t3); font-weight: 400 }
  dd { margin: 0; color: var(--t2); text-align: right }
  dd.warn { max-width: 40ch; color: var(--warn) }
  .need { display: block; margin-top: var(--s-1); max-width: 40ch; color: var(--warn) }
  .group { margin: var(--s-6) 0 var(--s-2); font-size: 11px; font-weight: 600; color: var(--t3) }
  .total { margin: 0; color: var(--t2); font-size: 12px }
  .model-backends { display: grid; gap: var(--s-3); margin-bottom: var(--s-6) }
  .model-backend { display: grid; grid-template-columns: minmax(125px, 1fr) minmax(0, 2fr); gap: var(--s-2) var(--s-5); align-items: center }
  .model-backend .sub { grid-column: 1 / -1; margin: 0 }
  @media (max-width: 620px) { .model-backend { grid-template-columns: 1fr } }
</style>
