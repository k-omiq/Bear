<script>
  /**
   * Setup step: which cleaning models to download.
   *
   * LaMa Manga is a download. FLUX models are not: they run in a separate
   * helper that brings its own weights, so their rows say whether the helper
   * lists them, and the one control under the table points at the helper's
   * folder. The rest are listed disabled until they can be fetched.
   */
  import { Button } from '../../ui/index.js'
  import { t } from '../../i18n/index.js'
  import { chooseFolder } from '../../api/folder.js'
  import { getBackend } from '../../api/backend.js'
  import { capabilities, loadCapabilities } from '../../state/capabilities.svelte.js'
  import { session, setSidecarPath } from '../../state/session.svelte.js'
  import EngineTable from '../EngineTable.svelte'
  import { CLEANERS, engineBytes } from '../../model/pipelines.js'
  import {
    chooseCleaner,
    finishedFiles,
    firstLaunch,
    loadFirstLaunchSidecarModels,
    saveFirstLaunchSetting,
  } from '../firstlaunch.svelte.js'

  let choosing = $state(false)
  let saveFailed = $state(false)
  let installing = $state(false)

  async function installFlux() {
    if (installing) return
    installing = true
    saveFailed = false
    try {
      await getBackend().installFluxHelper({ backend: 'auto', accelerator: 'auto' })
      await loadCapabilities()
      await loadFirstLaunchSidecarModels()
    } catch {
      saveFailed = true
    } finally {
      installing = false
    }
  }

  $effect(() => {
    if (capabilities.sidecar) loadFirstLaunchSidecarModels()
  })

  /** @param {import('../../model/pipelines.js').Engine} engine */
  function viaHelper(engine) {
    return Boolean(engine.sidecar === 'flux2-klein-4b' && capabilities.sidecar &&
      firstLaunch.sidecarModels.some((model) => model.id === engine.sidecar))
  }

  /** @param {import('../../model/pipelines.js').Engine} engine */
  function stateOf(engine) {
    if (engine.sidecar === 'flux2-klein-4b') return viaHelper(engine) ? t('pipelines.status.found') : t('pipelines.status.needsHelper')
    if (engine.cloudModel) return t('pipelines.status.cloudSetup')
    if (engine.sidecar) return t('pipelines.status.soon')
    if (!engine.ready) return t('pipelines.status.soon')
    const bytes = firstLaunch.plan ? engineBytes(engine, firstLaunch.plan.files, finishedFiles()) : 0
    return bytes > 0 ? t('models.value.size', { bytes }) : t('pipelines.status.installed')
  }

  async function browse() {
    if (choosing) return
    choosing = true
    try {
      /** @type {string|null} */
      let chosen = null
      try {
        chosen = await chooseFolder({
          title: t('settings.sidecar.chooserTitle'),
          defaultPath: session.sidecarPath || undefined,
        })
      } catch {
        // A chooser that could not open chose nothing, which is also what
        // closing it does.
        chosen = null
      }
      if (chosen !== null) saveFailed = !(await saveFirstLaunchSetting(setSidecarPath, chosen, session.sidecarPath))
    } finally {
      choosing = false
    }
  }
</script>

<p class="lead">{t('onboarding.cleaning.body')}</p>
<EngineTable
  engines={CLEANERS}
  label={t('pipelines.cleaning')}
  {stateOf}
  isAvailable={(engine) => engine.ready || viaHelper(engine)}
  chosen={firstLaunch.cleaners}
  onchoose={chooseCleaner}
/>

{#if !capabilities.sidecar}
  <div class="helper">
    <div class="text">
      <span class="name">{t('onboarding.cleaning.helper')}</span>
      <span class="about">{t('onboarding.cleaning.helperNote')}</span>
    </div>
    <Button size="sm" disabled={installing} onclick={installFlux}>
      {installing ? t('settings.sidecar.installing') : t('settings.sidecar.install')}
    </Button>
    <Button size="sm" disabled={choosing || installing} onclick={browse}>{t('shell.action.chooseFolder')}</Button>
  </div>
  {#if session.sidecarPath}
    <p class="alert" role="alert">{t('onboarding.cleaning.helperMissing')}</p>
  {/if}
{/if}
{#if saveFailed}<p class="alert" role="alert">{t('onboarding.saveFailed')}</p>{/if}

<style>
  .helper {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-6);
    margin-top: var(--s-6);
    padding-top: var(--s-5);
    border-top: 1px solid var(--line);
  }
  .text { display: flex; flex-direction: column; gap: 2px; min-width: 0 }
  .name { font-weight: 600 }
  .about { color: var(--t2); font-size: 11.5px; line-height: 1.45 }
</style>
