<script>
  /**
   * Setup step: which runtime build this computer gets, before it is
   * downloaded, and what the computer has for each graphics path.
   *
   * The first half of the Runtime screen (Settings, Runtime); the measurement
   * half is the `measure` step, after the downloads, because it needs the
   * models here. On a platform with one build there is nothing to choose and
   * the step says which build it is.
   */
  import { t } from '../../i18n/index.js'
  import { runtimeReady } from '../firstlaunch.js'
  import { chooseFirstLaunchFlavour, finishedFiles, firstLaunch, loadFirstLaunchAccelerators, runtimeDownloading } from '../firstlaunch.svelte.js'
  import BuildChoice from '../runtime/BuildChoice.svelte'
  import MachineCheck from '../runtime/MachineCheck.svelte'
  import { flavourName } from '../runtime/runtime.js'

  const plan = $derived(firstLaunch.plan)
  const flavours = $derived(plan?.runtime.flavours ?? [])
  const ready = $derived(runtimeReady(plan, finishedFiles(), firstLaunch.current))

  /** @type {string|null} */
  let found = $state(null)
  let saving = $state(false)
  let saveFailed = $state(false)

  // A runtime already here (a replayed setup) can say which paths it has.
  // Asked once each time it becomes ready: choosing another build clears the
  // paths and readiness with them, and choosing the installed one back makes
  // it ready again, with paths to ask for again.
  let asked = false
  $effect(() => {
    if (!ready) {
      asked = false
      return
    }
    if (asked || firstLaunch.accelerators) return
    asked = true
    loadFirstLaunchAccelerators()
  })

  /** @param {string} id */
  async function choose(id) {
    // The build holds still while it downloads; that is a wait, not a failure.
    if (saving || runtimeDownloading() || id === plan?.runtime.flavour) return
    saving = true
    saveFailed = !(await chooseFirstLaunchFlavour(id))
    saving = false
  }
</script>

<p class="lead">{t('onboarding.runtime.body')}</p>

<h2 class="group">{t('settings.runtime.heading.build')}</h2>
{#if flavours.length > 1}
  <BuildChoice {flavours} value={plan?.runtime.flavour ?? null} recommended={found} disabled={saving || runtimeDownloading()} onchoose={choose} />
{:else if plan?.runtimeUnavailable}
  <p class="line warn">{t('onboarding.dependencies.unavailable')}</p>
{:else}
  <p class="line">{t('onboarding.runtime.only', { build: flavourName(plan?.runtime.flavour), version: plan?.runtime.version ?? '' })}</p>
{/if}
{#if saveFailed}<p class="line warn" role="alert">{t('onboarding.saveFailed')}</p>{/if}

<h2 class="group">{t('settings.runtime.heading.machine')}</h2>
<MachineCheck
  flavour={plan?.runtime.flavour ?? null}
  {flavours}
  platform={plan?.platform ?? null}
  accelerators={firstLaunch.accelerators}
  pathsPending={!ready}
  onchooseflavour={choose}
  chooseDisabled={saving || runtimeDownloading()}
  onrecheck={loadFirstLaunchAccelerators}
  bind:found
/>

<style>
  .group { margin: var(--s-6) 0 var(--s-3); font-size: 11px; font-weight: 600; color: var(--t3) }
  .line { margin: 0; color: var(--t2); font-size: 12.5px }
  .warn { color: var(--warn) }
</style>
