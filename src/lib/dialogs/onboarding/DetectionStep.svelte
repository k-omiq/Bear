<script>
  /**
   * Setup step: which detector each language uses, or none, and whether the
   * optional text reader is wanted.
   *
   * *Detect on* leads (`DetectOnChoice`). On This computer the model boxes and
   * the reader are the user's to choose. On the Cloud GPU the combination is
   * fixed to all four (`pipelines.js#runDetection`): the boxes show it checked
   * and not editable, one note says why, and a line says setup downloads only
   * what that run needs here, CTD and the reader. The user's own boxes and
   * switch stay stored for This computer.
   *
   * A skipped language asks for no detection files, and the reader is off
   * until it is ticked, so a first setup fetches no OCR file. The table
   * below says what each engine is made of and what it costs, including the
   * ones that are not ready yet, so the choice reads as a roadmap. The
   * text-shaped review is not offered here: its graphs are imported later, in
   * Settings, and one line says so.
   */
  import { Select } from '../../ui/index.js'
  import { t } from '../../i18n/index.js'
  import { session, setDetectorModels } from '../../state/session.svelte.js'
  import { ALL_TEXT_POLICY, DETECTOR_CHOICES, HAYAI_FILES, LANGUAGES, engineBytes, model, runDetection, toggleDetectorModel } from '../../model/pipelines.js'
  import { chooseDetector, chooseOcrRescue, finishedFiles, firstLaunch } from '../firstlaunch.svelte.js'
  import DetectOnChoice from './DetectOnChoice.svelte'

  const uid = $props.id()

  /** The Select's value for "skip this language"; stored as `null`. */
  const SKIP = ''

  /** @param {string} language */
  function options(language) {
    return [
      { value: 'ctd-rtdetr', label: t('pipelines.clean') },
      { value: SKIP, label: t('pipelines.skip') },
    ]
  }

  /** Setup keeps the policy it finds; a replay after choosing all-text says so. */
  const allText = $derived(session.textPolicy === ALL_TEXT_POLICY)
  /** The models and reader a run uses: this computer's own, or the cloud GPU's fixed four. */
  const selection = $derived(runDetection(session))
  const cloudNow = $derived(selection.target === 'cloud')
  /** On the cloud GPU only its three models are listed: Small is not part of that run. */
  const listed = $derived(cloudNow ? DETECTOR_CHOICES.filter((id) => selection.detectorModels.includes(id)) : DETECTOR_CHOICES)

  /** The same rule Settings follows: Full and Small exclude each other, and one stays selected. */
  function chooseModel(id, checked) {
    setDetectorModels(toggleDetectorModel(session.detectorModels, id, checked))
  }

  /**
   * What ticking the text reader costs with these choices. It reads every
   * language under both text policies, so only its download is worth saying.
   */
  const rescueLine = $derived.by(() => {
    const plan = firstLaunch.plan
    const bytes = plan ? engineBytes({ files: HAYAI_FILES }, plan.files, finishedFiles()) : 0
    return bytes > 0 ? t('settings.detection.rescue.size', { bytes }) : ''
  })
</script>

<p class="lead">{t('onboarding.detection.body')}</p>
<DetectOnChoice />
<fieldset class="detection-models" aria-describedby={cloudNow ? `${uid}-combo` : undefined}>
  <legend>{t('settings.detection.modelsLegend')}</legend>
  {#each listed as id (id)}
    <label>
      <input type="checkbox" checked={selection.detectorModels.includes(id)}
        disabled={cloudNow || (session.detectorModels.length === 1 && session.detectorModels.includes(id))}
        onchange={(event) => chooseModel(id, event.currentTarget.checked)} />
      {model(id)?.product}
    </label>
  {/each}
</fieldset>
<!-- The Hayai text reader, right under the detection models it completes:
     opt-in here, ticked and fixed with them on the cloud GPU. The same switch
     Settings > Models shows; the description and the cost are read with the
     box. -->
<div class="rescue">
  <input
    id="{uid}-rescue"
    type="checkbox"
    checked={selection.ocrRescue}
    disabled={cloudNow}
    aria-describedby="{uid}-rescue-description {uid}-rescue-line"
    onchange={(event) => chooseOcrRescue(event.currentTarget.checked)}
  />
  <div class="rescue-text">
    <label for="{uid}-rescue">{t('pipelines.workflow.ocrRescue')}</label>
    <p id="{uid}-rescue-description">{t('pipelines.workflow.ocrRescueDescription')}</p>
    <p id="{uid}-rescue-line" class="rescue-line" role="status">{rescueLine}</p>
  </div>
</div>
{#if !cloudNow}
  <p class="note">{t('settings.detection.selectedModels', { models: [...session.detectorModels, ...(selection.ocrRescue ? ['hayaiOcr'] : [])].map((id) => model(id)?.product).filter(Boolean).join(' + ') })}</p>
{/if}
<p class="note" id="{uid}-combo" data-cloud-combo>{t('pipelines.cloudCombo')}</p>
{#if cloudNow}<p class="note policy" data-cloud-now>{t('onboarding.detection.cloudNow')}</p>{/if}
{#if allText}<p class="note policy">{t('settings.detection.setupAllText')}</p>{/if}
<div class="languages">
  {#each LANGUAGES as language (language.id)}
    <div class="language" class:skipped={!firstLaunch.detection[language.id]}>
      <span class="name">{t(language.labelKey)}</span>
      <Select
        options={options(language.id)}
        value={firstLaunch.detection[language.id] ?? SKIP}
        label={t('pipelines.detectorFor', { language: t(language.labelKey) })}
        onchange={(value) => chooseDetector(language.id, value || null)}
      />
    </div>
  {/each}
</div>
<p class="note">{t('settings.detection.setupReview')}</p>

<style>
  .detection-models { display: flex; flex-wrap: wrap; gap: var(--s-4); margin-bottom: var(--s-4); border: 0; padding: 0 }
  .detection-models legend { font-weight: 600; margin-bottom: var(--s-2) }
  .detection-models label { display: flex; align-items: center; gap: var(--s-2) }
  .policy { margin: 0 0 var(--s-5) }
  .languages { display: grid; gap: var(--s-2); margin-bottom: var(--s-5) }
  .language {
    display: grid;
    grid-template-columns: 110px minmax(0, 280px);
    align-items: center;
    gap: var(--s-5);
  }
  .name { font-weight: 600 }
  .skipped .name { color: var(--t3) }

  /* The engine table's checkbox shape: box left, title over its note. */
  .rescue {
    display: flex;
    align-items: flex-start;
    gap: var(--s-4);
    max-width: 62ch;
    margin-bottom: var(--s-5);
  }
  .rescue input { flex: none; margin: 2px 0 0; accent-color: var(--accent); cursor: pointer }
  .rescue-text { display: grid; gap: 2px; min-width: 0 }
  .rescue-text label { font-weight: 600; cursor: pointer }
  .rescue-text p { margin: 0; color: var(--t2); font-size: 11.5px; line-height: 1.45 }
</style>
