<script>
  /**
   * The denoise presets as one radio group: name, what it does, whose model
   * it is, and the time a page takes, on every row.
   *
   * Drawn by setup, Settings > Denoise and the chapter dialog, so the three
   * read the same list. The time is the caller's (`timeOf`): the dialog scales
   * cloud times by the chapter's own pages, the other two show the reference
   * page. The credit line is a licence obligation, not decoration, and is on
   * every row wherever the list is drawn.
   *
   * Above the rows, what the pages are: black and white or colour. A preset
   * is listed when it is made for that or for both (`presetsForPages`), so a
   * colour illustration model is not offered for a black and white chapter.
   * The filter starts on the chosen preset's own type and is this list's
   * alone, not a setting. Switching it away from the chosen preset chooses the
   * first one still listed, through `onchange` like any other choice; a preset
   * chosen from outside that the filter would hide moves the filter instead
   * (`pageKindFor`). A choice made in the list holds the filter where it is,
   * so picking a preset made for both never changes what is listed. It is left
   * out when every preset is for both, as on this computer, where it would
   * change nothing.
   *
   * A real radio group: one tab stop, arrows move and select, Home and End go
   * to the ends, all over the rows the filter shows. Stopped as well as
   * prevented, because the editor underneath pages the chapter on the arrows.
   *
   * @type {{
   *   presets: import('../../model/denoise.js').DenoisePreset[],
   *   value: string|null,
   *   onchange: (id: string) => void,
   *   label: string,
   *   timeOf: (preset: import('../../model/denoise.js').DenoisePreset) => {value: string, where: string}|null,
   *   disabled?: boolean,
   * }}
   */
  import { t } from '../../i18n/index.js'
  import { PAGE_KINDS, PRESET_TEXT, pageKindFor, presetsForPages } from '../../model/denoise.js'
  import { Segmented } from '../../ui/index.js'
  import Icon from '../../icons/Icon.svelte'

  let { presets, value, onchange, label, timeOf, disabled = false } = $props()
  const uid = $props.id()

  /** @type {HTMLButtonElement[]} */
  let rows = $state([])

  /**
   * The page type held here, by a pick on the filter or by a choice made in
   * the list, until a preset chosen elsewhere contradicts it. Null until
   * either: the chosen preset decides.
   */
  let picked = $state(/** @type {import('../../model/denoise.js').PageKind|null} */ (null))
  const pages = $derived(pageKindFor(presets, value, picked))
  const shown = $derived(presetsForPages(presets, pages))
  const filtered = $derived(presets.some((preset) => preset.kind === 'bw' || preset.kind === 'color'))
  const kinds = $derived(PAGE_KINDS.map((kind) => ({
    value: kind,
    label: t(kind === 'color' ? 'denoise.pages.color' : 'denoise.pages.bw'),
    disabled: !presetsForPages(presets, kind).length,
  })))

  const selected = $derived(shown.some((preset) => preset.id === value) ? value : shown[0]?.id ?? null)

  /** @param {string} kind */
  function choosePages(kind) {
    const next = presetsForPages(presets, kind)
    if (!next.length) return
    picked = /** @type {import('../../model/denoise.js').PageKind} */ (kind)
    if (!next.some((preset) => preset.id === selected)) onchange(next[0].id)
  }

  /** A choice made in the list: the list stays as it is shown. @param {string} id */
  function choose(id) {
    picked = pages
    onchange(id)
  }

  /** @param {string} id */
  function nameOf(id) {
    const text = /** @type {Record<string, {nameKey: string}>} */ (PRESET_TEXT)[id]
    return text ? t(text.nameKey) : id
  }

  /** @param {string} id */
  function noteOf(id) {
    const text = /** @type {Record<string, {noteKey: string}>} */ (PRESET_TEXT)[id]
    return text ? t(text.noteKey) : ''
  }

  /** @param {KeyboardEvent} event @param {number} index */
  function onkeydown(event, index) {
    let next = null
    if (event.key === 'ArrowDown' || event.key === 'ArrowRight') next = (index + 1) % shown.length
    else if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') next = (index - 1 + shown.length) % shown.length
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = shown.length - 1
    if (next === null) return
    event.preventDefault()
    event.stopPropagation()
    choose(shown[next].id)
    rows[next]?.focus()
  }
</script>

{#if filtered}
  <div class="pages">
    <span class="pages-label" id="{uid}-pages">{t('denoise.pages.label')}</span>
    <Segmented options={kinds} value={pages} labelledBy="{uid}-pages" {disabled} onchange={choosePages} />
  </div>
{/if}
<div class="presets" role="radiogroup" aria-label={label} aria-disabled={disabled || undefined} data-pages={pages}>
  {#each shown as preset, index (preset.id)}
    {@const on = preset.id === selected}
    {@const time = timeOf(preset)}
    <button
      bind:this={rows[index]}
      type="button"
      role="radio"
      class="preset"
      class:on
      aria-checked={on}
      tabindex={on ? 0 : -1}
      data-preset={preset.id}
      {disabled}
      onclick={() => choose(preset.id)}
      onkeydown={(event) => onkeydown(event, index)}
    >
      <span class="mark" aria-hidden="true">{#if on}<Icon name="check" size={12} />{/if}</span>
      <span class="text">
        <span class="name">{nameOf(preset.id)}</span>
        <span class="note">{noteOf(preset.id)}</span>
        <span class="credit">{t('denoise.credit', preset.credit)}</span>
      </span>
      <span class="time" data-time={time ? 'known' : 'unknown'}>
        {#if time}
          <span class="value">{time.value}</span>
          <span class="where">{time.where}</span>
        {:else}
          <span class="where">{t('denoise.time.notMeasured')}</span>
        {/if}
      </span>
    </button>
  {/each}
</div>

<style>
  .pages {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s-5);
    padding-bottom: var(--s-3);
  }
  .pages-label { font-size: 11px; color: var(--t3) }

  .presets { display: grid; border-top: 1px solid var(--line) }

  .preset {
    display: grid;
    grid-template-columns: 16px minmax(0, 1fr) auto;
    align-items: start;
    gap: var(--s-4);
    padding: var(--s-4) var(--s-3);
    border: none;
    border-bottom: 1px solid var(--line);
    border-radius: 0;
    background: transparent;
    color: var(--text);
    text-align: left;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease);
  }
  .preset:hover:not(:disabled) { background: var(--accent-soft) }
  .preset.on { background: var(--accent-soft) }
  .preset:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; border-radius: var(--r-sm) }
  .preset:disabled { cursor: default; opacity: .55 }

  .mark {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    margin-top: 1px;
    border-radius: var(--r-pill);
    box-shadow: inset 0 0 0 1.5px var(--line2);
    color: var(--accent-fg);
  }
  .on .mark { background: var(--accent); box-shadow: none }

  .text { display: flex; flex-direction: column; gap: 2px; min-width: 0 }
  .name { font-size: 12.5px; font-weight: 600 }
  .note { font-size: 11.5px; line-height: 1.45; color: var(--t2) }
  .credit { font-size: 11px; line-height: 1.4; color: var(--t3) }

  .time {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 2px;
    min-width: 88px;
    text-align: right;
  }
  .value { font-size: 12.5px; font-weight: 600; font-variant-numeric: tabular-nums; white-space: nowrap }
  .where { font-size: 11px; color: var(--t3); white-space: nowrap }

  @media (max-width: 480px) {
    .preset { grid-template-columns: 16px minmax(0, 1fr) }
    .time { grid-column: 2; flex-direction: row; align-items: baseline; gap: var(--s-2); text-align: left }
  }
</style>
