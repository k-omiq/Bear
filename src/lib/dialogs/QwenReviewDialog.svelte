<script>
  import { untrack } from 'svelte'
  import { Button, Modal } from '../ui/index.js'
  import { closeModal } from '../state/app.svelte.js'
  import { t } from '../i18n/index.js'
  let { spec } = $props()
  const uid = $props.id()
  const preview = untrack(() => spec.props.preview)
  let description = $state(untrack(() => spec.props.draftDescription ?? preview.edit?.description ?? ''))
  $effect(() => { spec.props.draftDescription = description })
</script>
<Modal title={t('qwen.review.title')} meta={t('notice.cloud.job.page', { page: (preview.pageIndex ?? 0) + 1 })} width={1000} blocking onclose={() => closeModal({ choice: 'discard' })}>
  <p>{t('qwen.review.help')}</p>
  <div class="compare">
    <figure><figcaption>{t('qwen.review.before')}</figcaption><img src={preview.before} alt={t('qwen.review.before')} /></figure>
    <figure><figcaption>{t('qwen.review.after')}</figcaption><img src={preview.after} alt={t('qwen.review.after')} /></figure>
  </div>
  {#if preview.canRetry}
    <label for={`${uid}-description`}>{t('qwen.prompt.description')}</label>
    <input id={`${uid}-description`} type="text" bind:value={description} maxlength={500} placeholder={t('qwen.prompt.example')} />
    <p class="hint">{t('qwen.review.retryCost')}</p>
  {/if}
  {#snippet buttons()}
    <Button onclick={() => closeModal({ choice: 'discard' })}>{t('qwen.review.discard')}</Button>
    {#if preview.canRetry}<Button disabled={description.trim() === (preview.edit?.description ?? '').trim()} onclick={() => closeModal({ choice: 'retry', edit: { target: preview.edit?.target ?? 'auto', description } })}>{t('qwen.review.retry')}</Button>{/if}
    <Button variant="primary" onclick={() => closeModal({ choice: 'use' })}>{t('qwen.review.use')}</Button>
  {/snippet}
</Modal>
<style>
  p { margin: 0 0 12px; color: var(--t2); }
  .compare { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-bottom: 16px; }
  figure { margin: 0; min-width: 0; }
  figcaption { margin-bottom: 8px; font-weight: 600; }
  img { display: block; width: 100%; max-height: 55vh; object-fit: contain; }
  label { display: block; font-weight: 600; margin-bottom: 8px; }
  input { box-sizing: border-box; width: 100%; padding: 10px; background: var(--surface); color: var(--text); border: 1px solid var(--line); border-radius: 5px; }
  .hint { margin-top: 8px; font-size: 12px; }
</style>
