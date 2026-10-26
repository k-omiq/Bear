<script>
  /**
   * Setup step, after the downloads: measure the fastest path for each model
   * here, and choose a backend per model where the user wants one.
   *
   * The second half of the Runtime screen (Settings, Runtime). Only the
   * models this setup's choices run on this computer are listed for a choice:
   * with Detect on set to Cloud GPU, that run's own CTD rather than this
   * computer's selection, and the Hayai reader whenever the run reads.
   */
  import { t } from '../../i18n/index.js'
  import { Button, Select } from '../../ui/index.js'
  import { writeSettingsSerialized } from '../../state/settingswrite.js'
  import { getBackend } from '../../api/backend.js'
  import { backendSettingsPatch, session, setModelAccelerator } from '../../state/session.svelte.js'
  import { localDetectorModels, rescueRuns, runDetection } from '../../model/pipelines.js'
  import { runtimeReady } from '../firstlaunch.js'
  import { finishedFiles, firstLaunch, loadFirstLaunchAccelerators } from '../firstlaunch.svelte.js'
  import Measurement from '../runtime/Measurement.svelte'
  import { backendName, modelName } from '../runtime/runtime.js'

  const plan = $derived(firstLaunch.plan)
  const runtimeHere = $derived(runtimeReady(plan, finishedFiles(), firstLaunch.current))

  let asked = false
  $effect(() => {
    if (!runtimeHere || asked || firstLaunch.accelerators) return
    asked = true
    loadFirstLaunchAccelerators()
  })

  let backendSaveFailed = $state(false)
  const reads = $derived(rescueRuns(session.detection, runDetection(session).ocrRescue))
  const selectedModels = $derived(new Set([...localDetectorModels(session), ...(reads ? ['hayai'] : []), 'inpainter']))
  const modelRows = $derived((firstLaunch.accelerators?.models ?? []).filter((row) => selectedModels.has(row.id)))

  function backendChoices(row) {
    const choices = [
      { value: 'inherit', label: t('settings.accel.inherit', { backend: t(session.accelerator === 'auto' ? 'settings.accel.auto' : `accel.${session.accelerator}`) }) },
      { value: 'auto', label: t('settings.accel.auto') },
    ]
    for (const status of row.backendStatus ?? []) {
      const level = status.verified ? 'verified' : status.available ? 'available' : status.installed ? 'installed' : status.supported ? 'supported' : 'unsupported'
      choices.push({
        value: status.id,
        label: `${backendName(status.id)} · ${status.reasonKey ? t(status.reasonKey) : t(`settings.accel.state.${level}`)}`,
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

  // A measurement that saved is not undone by a list that could not be read
  // after it: the rows keep the answer they had, and the next read fixes them.
  async function measured() {
    await loadFirstLaunchAccelerators()
  }
</script>

<p class="lead">{t('onboarding.measure.body')}</p>

<Measurement accelerators={firstLaunch.accelerators} ready={runtimeHere && Boolean(firstLaunch.accelerators)} onmeasured={measured} />
{#if runtimeHere && firstLaunch.acceleratorsFailed}
  <p class="need" role="alert">
    {t('settings.accel.unreadable')}
    <Button size="sm" variant="plain" onclick={loadFirstLaunchAccelerators}>{t('settings.runtime.machine.recheck')}</Button>
  </p>
{/if}

{#if modelRows.length}
  <h2 class="group">{t('settings.accel.models')}</h2>
  <p class="help">{t('settings.accel.modelHelp')}</p>
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

<style>
  .group { margin: var(--s-7) 0 var(--s-2); font-size: 11px; font-weight: 600; color: var(--t3) }
  .help { margin: 0 0 var(--s-4); color: var(--t2); font-size: 12px; max-width: 68ch }
  .need { margin: 0 0 var(--s-3); color: var(--warn) }
  .model-backends { display: grid; gap: var(--s-3) }
  .model-backend { display: grid; grid-template-columns: minmax(125px, 1fr) minmax(0, 2fr); gap: var(--s-2) var(--s-5); align-items: center }
  .model-backend .sub { grid-column: 1 / -1; margin: 0; color: var(--t3) }
  @media (max-width: 620px) { .model-backend { grid-template-columns: 1fr } }
</style>
