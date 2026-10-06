<script>
  /**
   * How long a page takes on this computer, and the button that finds out.
   *
   * The figure is only ever measured (`benchmark_denoise_local`), never
   * guessed: until it has run, the row says so and offers Measure. Measuring
   * needs the local model, so the button waits for it and says why.
   *
   * @type {{ installed: boolean }}
   */
  import { Button } from '../../ui/index.js'
  import { t } from '../../i18n/index.js'
  import { session } from '../../state/session.svelte.js'
  import { denoise, measureLocalDenoise } from '../../state/denoise.svelte.js'
  import { durationText, localSeconds } from '../../model/denoise.js'
  import Spinner from './Spinner.svelte'

  let { installed } = $props()
  const uid = $props.id()

  const seconds = $derived(localSeconds(session.denoiseLocalSecondsPerPage))
  const value = $derived.by(() => {
    if (seconds === null) return null
    const text = durationText(seconds)
    return t('denoise.time.local', { duration: t(text.key, text.params) })
  })
</script>

<div class="local-time" data-measured={seconds !== null}>
  <p class="figure" id="{uid}-figure" role="status">
    {#if denoise.measuring}
      <Spinner />
      <span>{t('denoise.time.measuring')}</span>
    {:else if value}
      <span class="value">{value}</span>
      <span class="where">{t('denoise.time.localWhere')}</span>
    {:else}
      <span class="where">{t('denoise.time.notMeasured')}</span>
    {/if}
  </p>
  <Button
    size="sm"
    disabled={denoise.measuring || !installed}
    aria-describedby={denoise.measureFailed || !installed ? `${uid}-why` : undefined}
    onclick={() => measureLocalDenoise()}
  >
    {seconds === null ? t('denoise.time.measure') : t('denoise.time.remeasure')}
  </Button>
</div>
{#if denoise.measureFailed}
  <p class="why failed" id="{uid}-why" role="alert">{t('denoise.time.measureFailed')}</p>
{:else if !installed}
  <p class="why" id="{uid}-why">{t('denoise.time.needsModel')}</p>
{/if}

<style>
  .local-time {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-5);
  }
  .figure {
    display: flex;
    align-items: baseline;
    gap: var(--s-2);
    margin: 0;
    min-width: 0;
    font-size: 12.5px;
    color: var(--text);
  }
  .figure :global(.spinner) { align-self: center }
  .value { font-weight: 600; font-variant-numeric: tabular-nums }
  .where { color: var(--t2); font-size: 12px }
  .why { margin: var(--s-2) 0 0; font-size: 11.5px; line-height: 1.45; color: var(--t3) }
  .why.failed { color: var(--warn) }
</style>
