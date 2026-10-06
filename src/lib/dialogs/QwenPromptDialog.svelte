<script>
  import { untrack } from 'svelte'
  import { Button, Modal } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { t } from '../i18n/index.js'
  let { spec } = $props()
  const uid = $props.id()
  const batch = untrack(() => spec.props?.batch === true)
  let target = $state(untrack(() => spec.props?.draft?.target ?? spec.props?.initial?.target ?? (batch ? 'auto' : 'sound_effect')))
  let description = $state(untrack(() => spec.props?.draft?.description ?? spec.props?.initial?.description ?? ''))
  $effect(() => { spec.props.draft = { target, description } })
  const labels = { auto: 'qwen.prompt.auto', dialogue: 'qwen.prompt.dialogue', sound_effect: 'qwen.prompt.soundEffect', other: 'qwen.prompt.otherText' }
  const choices = batch || untrack(() => spec.props?.initial?.target === 'auto')
    ? ['auto', 'dialogue', 'sound_effect', 'other'] : ['dialogue', 'sound_effect', 'other']
</script>
<Modal title={t('qwen.prompt.title')} width={520} blocking onclose={() => closeModal(null)}>
  <p>{t('qwen.prompt.help')}</p>
  {#if batch}<p>{t('qwen.prompt.batch')}</p>{/if}
  <fieldset>
    <legend>{t('qwen.prompt.target')}</legend>
    {#each choices as choice}
      <label><input type="radio" name={`${uid}-target`} value={choice} bind:group={target} />{t(labels[choice])}</label>
    {/each}
  </fieldset>
  <label class="description" for={`${uid}-description`}>{t('qwen.prompt.description')}</label>
  <input id={`${uid}-description`} type="text" bind:value={description} maxlength={500} placeholder={t('qwen.prompt.example')} />
  <p class="hint">{t('qwen.prompt.preserve')}</p>
  {#snippet buttons()}
    <Button onclick={() => closeModal(null)}>{t('qwen.prompt.cancel')}</Button>
    <Button variant="primary" onclick={() => closeModal({ target, description })}>{t('qwen.prompt.clean')}</Button>
  {/snippet}
</Modal>
<style>
  p { margin: 0 0 14px; color: var(--t2); }
  fieldset { border: 0; padding: 0; margin: 0 0 16px; display: flex; gap: 14px; flex-wrap: wrap; }
  legend { margin-bottom: 8px; font-weight: 600; }
  label { display: flex; align-items: center; gap: 6px; }
  .description { margin-bottom: 8px; font-weight: 600; }
  input[type='text'] { box-sizing: border-box; width: 100%; padding: 10px; background: var(--surface); color: var(--text); border: 1px solid var(--line); border-radius: 5px; }
  .hint { margin-top: 12px; font-size: 12px; }
</style>
