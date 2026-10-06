<script>
  /**
   * One pipeline's engines, with their two ratings and what each still costs.
   * Drawn by onboarding and by Settings, so both read the same table.
   *
   * With `onchoose`, ready engines get a checkbox; engines that are not ready
   * are drawn muted and cannot be chosen.
   *
   * **The stars are provisional**, and the table says so itself, beside them,
   * wherever it is drawn: setup and Settings both, Detection and Cleaning
   * both. The ratings come from file sizes and scattered timings rather than
   * one shared benchmark (`model/pipelines.js`), and a caption that each
   * caller had to remember was a caption two of the four places forgot.
   *
   * @type {{
   *   engines: readonly import('../model/pipelines.js').Engine[],
   *   label: string,
   *   stateOf: (engine: import('../model/pipelines.js').Engine) => string,
   *   isAvailable?: (engine: import('../model/pipelines.js').Engine) => boolean,
   *   chosen?: Record<string, boolean>,
   *   onchoose?: (id: string, wanted: boolean) => void,
   * }}
   */
  import { Stars } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  let { engines, label, stateOf, isAvailable = (engine) => engine.ready, chosen = {}, onchoose } = $props()
  const uid = $props.id()
</script>

<p class="provisional" id="{uid}-provisional">{t('pipelines.workflow.ratingsNote')}</p>
<div class="engines" role="table" aria-label={label} aria-describedby="{uid}-provisional">
  <div class="engine head" role="row">
    <span role="columnheader">{t('pipelines.column.engine')}</span>
    <span role="columnheader">{t('pipelines.column.efficiency')}</span>
    <span role="columnheader">{t('pipelines.column.light')}</span>
    <span role="columnheader" class="end">{t('pipelines.column.size')}</span>
  </div>
  {#each engines as engine (engine.id)}
    {@const available = isAvailable(engine)}
    <div class="engine" class:soon={!available} role="row">
      <span class="name" role="rowheader">
        {#if onchoose}
          <input
            type="checkbox"
            id="{uid}-{engine.id}"
            checked={chosen[engine.id] === true}
            disabled={!available}
            onchange={(event) => onchoose(engine.id, event.currentTarget.checked)}
          />
          <label for="{uid}-{engine.id}"><strong>{engine.name}</strong><small>{engine.noteKey ? t(engine.noteKey) : ''}</small></label>
        {:else}
          <span class="text"><strong>{engine.name}</strong><small>{engine.noteKey ? t(engine.noteKey) : ''}</small></span>
        {/if}
      </span>
      <span role="cell"><Stars value={engine.rating.efficiency} label={t('pipelines.rating.efficiency', { value: engine.rating.efficiency })} /></span>
      <span role="cell"><Stars value={engine.rating.light} label={t('pipelines.rating.light', { value: engine.rating.light })} /></span>
      <span role="cell" class="end state">{stateOf(engine)}</span>
    </div>
  {/each}
</div>

<style>
  .provisional {
    margin: 0 0 var(--s-3);
    font-size: 11px;
    line-height: 1.45;
    color: var(--t3);
    max-width: 68ch;
  }
  .engines { display: grid }
  .engine {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 84px 84px 84px;
    align-items: center;
    gap: var(--s-4);
    padding: var(--s-4) 0;
    border-top: 1px solid var(--line);
  }
  .engine.head {
    padding: 0 0 var(--s-3);
    border-top: none;
    color: var(--t3);
    font-size: 11px;
  }
  .name { display: flex; align-items: center; gap: var(--s-4); min-width: 0 }
  .name input { flex: none; margin: 0; accent-color: var(--accent) }
  .name label, .name .text { display: flex; flex-direction: column; gap: 2px; min-width: 0 }
  .name label { cursor: pointer }
  .name strong { font-weight: 600 }
  .name small { color: var(--t2); font-size: 11.5px }
  .soon strong, .soon small { color: var(--t3) }
  .soon :global(.stars) { opacity: .45 }
  .end { text-align: right }
  .state { color: var(--t2); font-size: 11.5px }
</style>
