<script>
  /**
   * Settings > Denoise: where page denoise runs, the preset, the local model
   * and how long a page takes on this computer.
   *
   * Every change is saved at once through `saveDenoiseChoice`, the same path
   * setup takes, and the preset list follows the target: a target never
   * shows a preset it cannot run. Cloud is offered only while a cloud GPU is
   * set up and allowed and its deployment has page denoise (`cloudOffered`),
   * and says why otherwise. The deployment is asked each time this tab opens
   * (`active`), since setup in the Cloud tab can change the answer.
   *
   * The local model and its time are shown whatever the target, so the model
   * can be fetched and measured before Local is chosen. Its row reads
   * Settings' own catalogue (`view`, `refresh`) rather than asking again.
   *
   * @type {{
   *   view?: import('../../api/backend.js').ModelsView|null,
   *   refresh?: () => Promise<unknown>,
   *   active?: boolean,
   * }}
   */
  import { onMount, untrack } from 'svelte'
  import { Field, Segmented } from '../../ui/index.js'
  import { t } from '../../i18n/index.js'
  import { session } from '../../state/session.svelte.js'
  import { cloud } from '../../state/cloud.svelte.js'
  import { configurationKey } from '../../state/cloudconfig.svelte.js'
  import {
    checkCloudDenoise, cloudNote, cloudOffered, currentPreset, loadDenoisePresets, offeredPresets, saveDenoiseChoice,
  } from '../../state/denoise.svelte.js'
  import PresetList from './PresetList.svelte'
  import LocalTime from './LocalTime.svelte'
  import ModelStatus from './ModelStatus.svelte'
  import { presetTime } from './time.js'

  let { view = undefined, refresh = undefined, active = true } = $props()
  const uid = $props.id()

  let installed = $state(false)
  let saveFailed = $state(false)

  onMount(() => {
    loadDenoisePresets()
  })

  // Ask again when the tab opens and when cloud use is switched.
  $effect(() => {
    void session.cloudAllowed
    void configurationKey(cloud.readiness)
    if (active) untrack(checkCloudDenoise)
  })

  const target = $derived(session.denoiseTarget)
  const note = $derived(cloudNote())
  const presets = $derived(offeredPresets(target))
  const preset = $derived(currentPreset(target))

  const targets = $derived([
    { value: 'local', label: t('denoise.target.local') },
    { value: 'cloud', label: t('denoise.target.cloud'), disabled: !cloudOffered() },
    { value: 'off', label: t('denoise.target.off') },
  ])

  /** @param {{target?: string, preset?: string}} choice */
  async function save(choice) {
    saveFailed = !(await saveDenoiseChoice(choice))
  }
</script>

<div class="denoise-settings">
  <p class="intro">{t('settings.denoise.intro')}</p>

  <Field label={t('settings.denoise.where')} layout="row">
    {#snippet children({ labelId })}
      <Segmented
        options={targets}
        value={target}
        labelledBy={labelId}
        describedBy={note ? `${uid}-cloud-why` : undefined}
        onchange={(value) => save({ target: value })}
      />
    {/snippet}
  </Field>
  {#if note}
    <p class="line" class:failed={target === 'cloud'} id="{uid}-cloud-why">{t(note)}</p>
  {/if}
  {#if saveFailed}<p class="line failed" role="alert">{t('onboarding.saveFailed')}</p>{/if}

  <section class="group" aria-labelledby="{uid}-preset">
    <h3 class="sub" id="{uid}-preset">{t('settings.denoise.preset')}</h3>
    {#if target === 'off'}
      <p class="line">{t('settings.denoise.offNote')}</p>
    {:else}
      <PresetList
        {presets}
        value={preset}
        label={t('settings.denoise.preset')}
        timeOf={(entry) => presetTime(entry, target, { localPerPage: session.denoiseLocalSecondsPerPage })}
        onchange={(id) => save({ preset: id })}
      />
    {/if}
  </section>

  <section class="group" aria-labelledby="{uid}-local">
    <h3 class="sub" id="{uid}-local">{t('settings.denoise.localModel')}</h3>
    <p class="line lead">{t('settings.denoise.localModelNote')}</p>
    <div class="block"><ModelStatus bind:installed {view} {refresh} /></div>
    <h4 class="minor">{t('settings.denoise.localTime')}</h4>
    <p class="line lead">{t('settings.denoise.localTimeNote')}</p>
    <div class="block"><LocalTime {installed} /></div>
  </section>
</div>

<style>
  /* Settings' own rhythm (`SettingsDialog.svelte`): a 12.5px semibold group
     heading, 11px notes in the third text colour, hairlines between blocks. */
  .intro {
    margin: calc(-1 * var(--s-3)) 0 var(--s-5);
    max-width: 68ch;
    font-size: 12px;
    line-height: 1.55;
    color: var(--t2);
  }
  .group { margin-top: var(--s-8) }
  .sub { margin: 0 0 var(--s-3); font-size: 12.5px; font-weight: 600 }
  .minor { margin: var(--s-6) 0 0; font-size: 12px; font-weight: 600 }
  .line {
    margin: var(--s-2) 0 0;
    max-width: 68ch;
    font-size: 11px;
    line-height: 1.45;
    color: var(--t3);
  }
  .line.lead { margin: 0 0 var(--s-3) }
  .line.failed { color: var(--warn) }
  .block { padding: var(--s-3) 0 var(--s-5); border-bottom: 1px solid var(--line) }
  .denoise-settings :global(.field.row .line) { min-height: 40px }
  .denoise-settings :global(.field.row .label) { font-size: 12.5px }
</style>
