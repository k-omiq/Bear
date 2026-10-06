<script>
  /**
   * Setup step: where page denoise runs, then which preset.
   *
   * Three cards: this computer, the cloud GPU, or not at all. This step comes
   * before the cloud step, so Cloud is offered with no cloud GPU yet: the
   * choice is what the cloud step then sets up or updates, with page denoise
   * ticked (`CloudProvisioner`'s `wantDenoise`).
   * The choice is saved at once, as every setup step saves, and it decides
   * the downloads: this computer adds the local model to the Downloads step
   * (`chosenFiles`), the other two download nothing.
   *
   * Under a target, its presets with the time a page takes. Cloud times are
   * the measured ones for the reference page on a cloud GPU; a local time
   * exists only once it has been measured on this computer, which needs the
   * model, so before the download the rows say so.
   */
  import { onMount } from 'svelte'
  import { t } from '../../i18n/index.js'
  import { session } from '../../state/session.svelte.js'
  import {
    checkCloudDenoise, currentPreset, denoise, loadDenoisePresets, offeredPresets, saveDenoiseChoice,
  } from '../../state/denoise.svelte.js'
  import { finishedFiles, firstLaunch } from '../firstlaunch.svelte.js'
  import PresetList from '../denoise/PresetList.svelte'
  import LocalTime from '../denoise/LocalTime.svelte'
  import { presetTime } from '../denoise/time.js'

  let saveFailed = $state(false)

  onMount(() => {
    loadDenoisePresets()
    checkCloudDenoise()
  })

  const CHOICES = {
    local: { nameKey: 'onboarding.denoise.local', noteKey: 'onboarding.denoise.localNote' },
    cloud: { nameKey: 'onboarding.denoise.cloud', noteKey: 'onboarding.denoise.cloudNote' },
    off: { nameKey: 'onboarding.denoise.off', noteKey: 'onboarding.denoise.offNote' },
  }

  const options = ['local', 'cloud', 'off']
  const target = $derived(options.includes(session.denoiseTarget) ? session.denoiseTarget : 'off')
  /** What the cloud step has to do for Cloud: nothing, a new setup, or an update that adds page denoise. */
  const cloudWork = $derived(
    target !== 'cloud' || (session.cloudAllowed && denoise.cloud === 'ready') ? null
      : session.cloudAllowed && denoise.cloud === 'missing' ? 'onboarding.denoise.cloudAdd'
        : !session.cloudAllowed ? 'onboarding.denoise.cloudNext' : null,
  )
  const presets = $derived(offeredPresets(target))
  const preset = $derived(currentPreset(target))

  /** What the local model still costs, and whether it is here already. */
  const local = $derived.by(() => {
    const plan = firstLaunch.plan
    const ids = plan?.denoise ?? []
    const done = finishedFiles()
    const missing = ids.filter((id) => !plan?.files[id]?.installed && !done[id])
    const bytes = missing.reduce((sum, id) => sum + (plan?.files[id]?.bytes ?? 0), 0)
    return { known: ids.length > 0, installed: ids.length > 0 && missing.length === 0, bytes }
  })

  /** @param {string} next */
  async function choose(next) {
    saveFailed = !(await saveDenoiseChoice({ target: next }))
  }

  /** @param {string} id */
  async function choosePreset(id) {
    saveFailed = !(await saveDenoiseChoice({ preset: id }))
  }

  /**
   * Arrows move and select, Home and End go to the ends. Selection follows
   * focus, as in every radio group in setup.
   *
   * @param {KeyboardEvent & {currentTarget: HTMLElement}} event
   * @param {number} index
   */
  function onkeydown(event, index) {
    let next = null
    if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (index + 1) % options.length
    else if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (index - 1 + options.length) % options.length
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = options.length - 1
    if (next === null) return
    event.preventDefault()
    event.stopPropagation()
    choose(options[next])
    const card = event.currentTarget.parentElement?.children[next]
    if (card instanceof HTMLElement) card.focus()
  }
</script>

<p class="lead">{t('onboarding.denoise.body')}</p>

<div class="choices" role="radiogroup" aria-label={t('onboarding.denoise.heading')}>
  {#each options as option, index (option)}
    {@const text = CHOICES[/** @type {keyof typeof CHOICES} */ (option)]}
    <button
      type="button"
      role="radio"
      class="choice"
      class:on={target === option}
      aria-checked={target === option}
      tabindex={target === option ? 0 : -1}
      data-target={option}
      onclick={() => choose(option)}
      onkeydown={(event) => onkeydown(event, index)}
    >
      <span class="name">{t(text.nameKey)}</span>
      <span class="about">{t(text.noteKey)}</span>
    </button>
  {/each}
</div>
{#if cloudWork}<p class="note" data-cloud-next>{t(cloudWork)}</p>{/if}
{#if saveFailed}<p class="alert" role="alert">{t('onboarding.saveFailed')}</p>{/if}

{#if target !== 'off'}
  <h2 class="presets-heading">{t('onboarding.denoise.presets')}</h2>
  <PresetList
    {presets}
    value={preset}
    label={t('onboarding.denoise.presets')}
    timeOf={(entry) => presetTime(entry, target, { localPerPage: session.denoiseLocalSecondsPerPage })}
    onchange={choosePreset}
  />
  {#if target === 'local'}
    {#if local.installed}
      <p class="note">{t('onboarding.denoise.installed')}</p>
      <div class="measure"><LocalTime installed /></div>
    {:else}
      {#if local.known}
        <p class="note" data-denoise-download>{t('onboarding.denoise.download', { bytes: local.bytes })}</p>
      {/if}
      <p class="note">{t('onboarding.denoise.measureLater')}</p>
    {/if}
  {/if}
{/if}

<style>
  .choices { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: var(--s-4) }
  @media (max-width: 620px) { .choices { grid-template-columns: 1fr } }
  .choice {
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    padding: var(--s-5) var(--s-5) var(--s-6);
    border: none;
    border-radius: var(--r-xl);
    background: var(--panel);
    box-shadow: 0 0 0 1px var(--line);
    color: var(--text);
    text-align: left;
    cursor: pointer;
    transition: box-shadow var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease);
  }
  .choice:hover { box-shadow: 0 0 0 1px var(--line2) }
  .choice.on { background: var(--accent-soft); box-shadow: 0 0 0 1.5px var(--accent) }
  .choice:active { transform: scale(.99) }
  .name { font-size: 13px; font-weight: 600 }
  .about { color: var(--t2); font-size: 12px; line-height: 1.5 }

  .presets-heading {
    margin: var(--s-8) 0 var(--s-3);
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text);
  }
  .measure { margin-top: var(--s-4) }
</style>
