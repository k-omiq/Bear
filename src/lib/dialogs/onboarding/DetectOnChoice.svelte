<script>
  /**
   * Setup's *Detect on*: This computer or Cloud GPU, the same choice the
   * editor's Text cleanup panel holds (`state/cloudtargets.svelte.js`).
   *
   * Cloud GPU is offered once the cloud can be used or is set up
   * (`cloudDetectOffered`); before that only This computer is, with one line
   * saying where the cloud is set up. The Detection step draws it, and so
   * does the Cloud step once its setup has turned cloud engines on, because
   * that step comes after Detection and a first-run user who set up a cloud
   * GPU there would otherwise never be asked.
   */
  import { t } from '../../i18n/index.js'
  import { detectTarget } from '../../state/cloudtargets.svelte.js'
  import { chooseFirstLaunchDetectTarget, cloudDetectOffered, firstLaunch } from '../firstlaunch.svelte.js'

  const uid = $props.id()

  const current = $derived(detectTarget())
  const offered = $derived(cloudDetectOffered())
  const options = $derived([
    { value: 'local', labelKey: 'tools.option.onLocal' },
    ...(offered ? [{ value: 'cloud', labelKey: 'tools.option.onCloud' }] : []),
  ])
  const describedBy = $derived(
    [!offered ? `${uid}-later` : null, firstLaunch.detectSaveFailed ? `${uid}-failed` : null].filter(Boolean).join(' ') || undefined,
  )
</script>

<fieldset class="detect-on" data-detect-on aria-describedby={describedBy}>
  <legend>{t('tools.param.detectOn')}</legend>
  <div class="places">
    {#each options as option (option.value)}
      <label>
        <input
          type="radio"
          name="{uid}-detect-on"
          value={option.value}
          checked={current === option.value}
          onchange={() => chooseFirstLaunchDetectTarget(/** @type {'local'|'cloud'} */ (option.value))}
        />
        {t(option.labelKey)}
      </label>
    {/each}
  </div>
  {#if !offered}<p class="note" id="{uid}-later">{t('onboarding.detection.cloudLater')}</p>{/if}
  {#if firstLaunch.detectSaveFailed}<p class="alert" role="alert" id="{uid}-failed">{t('tools.target.detectSaveFailed')}</p>{/if}
</fieldset>

<style>
  .detect-on { border: 0; padding: 0; margin: 0 0 var(--s-5) }
  .detect-on legend { font-weight: 600; margin-bottom: var(--s-2) }
  .places { display: flex; flex-wrap: wrap; gap: var(--s-5) }
  .places label { display: flex; align-items: center; gap: var(--s-2); cursor: pointer }
  .places input { margin: 0; accent-color: var(--accent); cursor: pointer }
  .note { margin: var(--s-2) 0 0 }
  .alert { margin: var(--s-2) 0 0 }
</style>
