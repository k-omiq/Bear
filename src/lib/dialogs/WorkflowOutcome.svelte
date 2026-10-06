<script>
  /**
   * One named outcome of the text-shaped review, local or cloud: a title, the
   * sentence that says what happened and what to do, and the native text
   * folded away under "Technical detail". The tone is carried by the icon's
   * shape as well as its color, so it never rests on hue alone.
   *
   * `action` is the one recovery that fits the outcome, when the caller has
   * one (Rebuild preview, Analyze again).
   */
  import { t } from '../i18n/index.js'
  import Icon from '../icons/Icon.svelte'
  import { outcomeCopy, toneOf } from './workflowoutcome.js'

  /** @type {{ outcome: import('./workflowoutcome.js').Outcome, action?: import('svelte').Snippet | null }} */
  let { outcome, action = null } = $props()

  const copy = $derived(outcomeCopy(outcome))
  const tone = $derived(toneOf(outcome.kind))
  const icon = $derived(tone === 'ok' ? 'check' : tone === 'info' ? 'info' : 'warning-triangle')
</script>

<div class="outcome {tone}" data-outcome={outcome.kind} data-reason={outcome.reason ?? null}>
  <span class="outcome-icon"><Icon name={icon} size={14} /></span>
  <div class="outcome-text">
    <strong>{t(copy.title, copy.params)}</strong>
    <span>{t(copy.body, copy.params)}</span>
    {#if outcome.detail}
      <details class="technical">
        <summary>{t('workflow.detail.technical')}</summary>
        <code>{outcome.detail}</code>
      </details>
    {/if}
  </div>
  {#if action}{@render action()}{/if}
</div>

<style>
  .outcome {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: start;
    gap: var(--s-3);
    padding: 8px 10px;
    border: 1px solid var(--line2);
    border-radius: var(--r-md);
    background: var(--panel2);
  }
  .outcome-icon { display: inline-flex; padding-top: 2px; color: var(--t2) }
  .outcome.warn .outcome-icon { color: var(--warn) }
  .outcome-text { display: grid; gap: 2px; min-width: 0; font-size: 12px; color: var(--t2); line-height: 1.45 }
  .outcome-text strong { color: var(--text); font-weight: 600 }
  .outcome-text span { overflow-wrap: anywhere }
  .technical { margin-top: 2px; font-size: 11px; color: var(--t2) }
  .technical > summary { cursor: pointer; width: max-content }
  .technical code { display: block; margin-top: 3px; font-size: 11px; color: var(--t2); white-space: pre-wrap; overflow-wrap: anywhere }
</style>
