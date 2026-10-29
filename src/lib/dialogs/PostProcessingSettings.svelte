<script>
  /**
   * Settings > Post-processing: how a cleaned patch is matched to the page
   * before it is written (`postprocess.rs#Finishing`).
   *
   * Three rows: the method, whether it also takes the place of the tone fix
   * on other models' results (Qwen, FLUX.2 Klein, LaMa, and FLUX run on this
   * computer; a FLUX 3 result is always finished), and whether a colour
   * result is made grey first. Each change
   * is saved at once through `saveCloudFinishing`; the backend freezes the
   * values on each cloud attempt and reads them when a run opens, so a change
   * applies to the next clean. A layer already cleaned is changed from its
   * own previews instead, in the Post-processing window. LaMa and local FLUX
   * run on this computer, so none of this needs Cloud GPU on.
   *
   * @type {{ backend?: import('../api/backend.js').Backend }}
   */
  import { untrack } from 'svelte'
  import { Field, Select } from '../ui/index.js'
  import { getBackend } from '../api/backend.js'
  import { session } from '../state/session.svelte.js'
  import { saveCloudFinishing } from '../state/cloud.svelte.js'
  import { FINISHINGS, finishingKey } from '../model/model-names.js'
  import { t } from '../i18n/index.js'

  let { backend: givenBackend } = $props()
  const backend = untrack(() => givenBackend ?? getBackend())

  const uid = $props.id()
  const ids = {
    method: `${uid}-method`,
    everyModel: `${uid}-every-model`,
    grayscale: `${uid}-grayscale`,
  }

  const methods = FINISHINGS.map((name) => ({ value: name, label: t(finishingKey('option', name)) }))
</script>

<p class="intro">{t('settings.postprocessing.description')}</p>

<Field
  label={t('settings.postprocessing.method')}
  description={t(finishingKey('hint', session.cloudFinishing))}
  layout="row"
  controlId={ids.method}
>
  {#snippet children({ descriptionId })}
    <Select
      id={ids.method}
      options={methods}
      value={session.cloudFinishing}
      describedBy={descriptionId}
      onchange={(next) => void saveCloudFinishing({ finishing: /** @type {any} */ (next) }, backend)}
    />
  {/snippet}
</Field>

<Field
  label={t('settings.postprocessing.everyModel')}
  description={t('settings.postprocessing.everyModelHint')}
  layout="row"
  controlId={ids.everyModel}
>
  {#snippet children({ descriptionId })}
    <input
      id={ids.everyModel}
      class="check"
      type="checkbox"
      aria-describedby={descriptionId}
      checked={session.finishingEveryModel}
      onchange={(event) => void saveCloudFinishing({ everyModel: event.currentTarget.checked }, backend)}
    />
  {/snippet}
</Field>

<Field
  label={t('settings.postprocessing.grayscale')}
  description={t('settings.postprocessing.grayscaleHint')}
  layout="row"
  controlId={ids.grayscale}
>
  {#snippet children({ descriptionId })}
    <input
      id={ids.grayscale}
      class="check"
      type="checkbox"
      aria-describedby={descriptionId}
      checked={session.cloudFinishingGrayscale}
      onchange={(event) => void saveCloudFinishing({ grayscale: event.currentTarget.checked }, backend)}
    />
  {/snippet}
</Field>

<style>
  .intro {
    margin: 0 0 var(--s-2);
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--t3);
  }
  .check {
    width: 15px;
    height: 15px;
    margin: 0;
    accent-color: var(--accent);
    cursor: pointer;
  }
</style>
