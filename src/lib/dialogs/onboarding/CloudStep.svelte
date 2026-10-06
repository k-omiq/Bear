<script>
  /**
   * Setup step: the optional cloud GPU, after Denoise so a cloud page denoise
   * chosen there is set up here.
   *
   * Two lines, and the dialog's two buttons: Not now leaves cloud cleaning
   * off, Set up now puts the provisioner in place of the lines (IC-5). The
   * provisioner owns everything about the setup itself, secrets included;
   * what comes back here is only that it finished and whether its endpoint
   * answered the first check, the permission that a passed check turns on
   * (`configureFirstLaunchCloud`), and whether it is busy in the user's
   * account, which the dialog reads to stay open. An endpoint that did not
   * answer is said back too, with where to test it, and is not offered for
   * setup again.
   *
   * Once cloud engines are on, *Detect on* is asked here as well as in the
   * Detection step (`DetectOnChoice`): this step comes after that one, so a
   * first-run user who sets up a cloud GPU here would otherwise never be
   * offered detection on it, nor its downloads.
   */
  import { tick, untrack } from 'svelte'
  import { t } from '../../i18n/index.js'
  import { session } from '../../state/session.svelte.js'
  import { focusable } from '../../ui/focus.js'
  import CloudProvisioner from '../CloudProvisioner.svelte'
  import DetectOnChoice from './DetectOnChoice.svelte'
  import { checkCloudDenoise, denoise } from '../../state/denoise.svelte.js'
  import {
    closeFirstLaunchProvisioner,
    configureFirstLaunchCloud,
    firstLaunch,
    setFirstLaunchProvisionerBusy,
  } from '../firstlaunch.svelte.js'

  /** @type {HTMLElement|undefined} */
  let panel = $state()

  // The provisioner replaces the step's content, so focus goes into it rather
  // than staying on a button that is no longer there.
  $effect(() => {
    if (!firstLaunch.provisioning) return
    tick().then(() => {
      if (panel) (focusable(panel)[0] ?? panel).focus()
    })
  })

  /** The Denoise step before this one chose the cloud GPU: setup ticks page denoise. */
  const wantDenoise = $derived(session.denoiseTarget === 'cloud')

  // Ask the cloud GPU what it runs on arrival and after each setup, so the
  // step knows whether the chosen page denoise still has to be added.
  $effect(() => {
    if (firstLaunch.provisioning || !session.cloudAllowed) return
    untrack(() => checkCloudDenoise())
  })

  const status = $derived.by(() => {
    const saved = firstLaunch.cloud
    if (saved?.healthy) {
      return saved.name ? t('onboarding.cloud.ready', { name: saved.name }) : t('onboarding.cloud.readyUnnamed')
    }
    if (saved) {
      return saved.name
        ? t('onboarding.cloud.unchecked', { name: saved.name })
        : t('onboarding.cloud.uncheckedUnnamed')
    }
    if (!session.cloudAllowed) return ''
    return wantDenoise && denoise.cloud === 'missing' ? t('onboarding.cloud.addDenoise') : t('onboarding.cloud.on')
  })
</script>

{#if firstLaunch.provisioning}
  <div class="provisioner" bind:this={panel} tabindex="-1">
    <CloudProvisioner
      inline
      initialProvider="modal"
      {wantDenoise}
      onclose={closeFirstLaunchProvisioner}
      onconfigured={configureFirstLaunchCloud}
      onbusychange={setFirstLaunchProvisionerBusy}
    />
  </div>
{:else}
  <p class="lead">{t('onboarding.cloud.body')}</p>
  <p class="lead">{t('onboarding.cloud.consent')}</p>
  {#if wantDenoise && !session.cloudAllowed && !firstLaunch.cloud}<p class="note" data-with-denoise>{t('onboarding.cloud.withDenoise')}</p>{/if}
  {#if firstLaunch.cloudSaveFailed}
    <p class="alert" role="alert">{t('onboarding.cloud.saveFailed')}</p>
  {:else if status}
    <p class="note" role="status">{status}</p>
  {/if}
  {#if session.cloudAllowed}<DetectOnChoice />{/if}
{/if}

<style>
  .provisioner { outline: none }
</style>
