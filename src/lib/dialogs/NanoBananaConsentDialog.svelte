<script>
  /**
   * The window in front of a Nano Banana request: the model and size, the
   * profile whose key pays (`props.profile`), what it may cost, that the crops go to Google, and a box where the person says what to
   * edit. Pushed by `editor/nanobananaask.svelte.js` with `blocking: true`, so
   * Escape is Cancel and the backdrop is inert.
   *
   * With Restrictive crop on (`props.restrictive`) it says that only small
   * areas around each region are sent, and the rest of the crop as white.
   *
   * Answers `{ id, prompt }`. The prompt is plain text the native side cleans and
   * adds under its own removal prompt.
   *
   * Focus lands in the box, so the count and the cost are the dialog's
   * description: a screen reader reads them with the title as it opens.
   */
  import { untrack } from 'svelte'
  import { Button, Modal } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { MAX_INSTRUCTION_CHARS, nanoBananaCost, nanoBananaModel } from '../model/nanobanana.js'
  import { t } from '../i18n/index.js'

  /** @type {{ spec: import('../state/app.svelte.js').ModalSpec }} */
  let { spec } = $props()

  const uid = $props.id()
  const facts = untrack(() => /** @type {any} */ (spec?.props ?? {}))
  const actions = untrack(() => spec?.actions ?? [])
  const model = nanoBananaModel(facts.model)
  const cost = nanoBananaCost(facts.count, facts.model)
  const money = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', minimumFractionDigits: 2 })
  let prompt = $state(String(facts.prompt ?? ''))
</script>

<Modal title={t('nanoBanana.run.title')} width={480} blocking onclose={() => closeModal(null)}
  describedby="{uid}-regions {uid}-cost">
  <p>{t('nanoBanana.run.model', { model: t(model.labelKey), size: facts.size })}</p>
  {#if typeof facts.profile === 'string'}
    <p data-profile>{t('nanoBanana.run.profile', { name: facts.profile })}</p>
  {/if}
  <p id="{uid}-regions">{t('nanoBanana.run.regions', { count: facts.count })}</p>
  <p id="{uid}-cost">{t('nanoBanana.run.cost', { low: money.format(cost.low), high: money.format(cost.high) })}</p>
  <label class="prompt" for="{uid}-prompt">{t('nanoBanana.run.promptLabel')}</label>
  <textarea
    id="{uid}-prompt"
    rows="3"
    maxlength={MAX_INSTRUCTION_CHARS}
    spellcheck="true"
    aria-describedby="{uid}-prompt-hint"
    placeholder={t('nanoBanana.run.promptPlaceholder')}
    bind:value={prompt}
  ></textarea>
  <p class="hint" id="{uid}-prompt-hint">{t('nanoBanana.run.promptHint')}</p>
  <p>{t('nanoBanana.run.sent')}</p>
  {#if facts.restrictive === true}
    <p data-restrictive>{t('nanoBanana.run.restrictive')}</p>
  {/if}
  {#snippet buttons()}
    {#each actions as action (action.id)}
      <Button variant={action.variant ?? 'ghost'} onclick={() => closeModal({ id: action.id, prompt })}>{t(action.labelKey)}</Button>
    {/each}
  {/snippet}
</Modal>

<style>
  .prompt {
    display: block;
    margin-top: 10px;
    font-weight: 600;
  }
  textarea {
    box-sizing: border-box;
    width: 100%;
    margin-top: 4px;
    resize: vertical;
    font: inherit;
  }
  .hint {
    margin: 4px 0 10px;
    opacity: 0.75;
    font-size: 0.9em;
  }
</style>
