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
   * A real radio group: one tab stop, arrows move and select, Home and End go
   * to the ends. Stopped as well as prevented, because the editor underneath
   * pages the chapter on the arrows.
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
  import { PRESET_TEXT } from '../../model/denoise.js'
  import Icon from '../../icons/Icon.svelte'

  let { presets, value, onchange, label, timeOf, disabled = false } = $props()

  /** @type {HTMLButtonElement[]} */
  let rows = $state([])

  const selected = $derived(presets.some((preset) => preset.id === value) ? value : presets[0]?.id ?? null)

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
    if (event.key === 'ArrowDown' || event.key === 'ArrowRight') next = (index + 1) % presets.length
    else if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') next = (index - 1 + presets.length) % presets.length
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = presets.length - 1
    if (next === null) return
    event.preventDefault()
    event.stopPropagation()
    onchange(presets[next].id)
    rows[next]?.focus()
  }
</script>

<div class="presets" role="radiogroup" aria-label={label} aria-disabled={disabled || undefined}>
  {#each presets as preset, index (preset.id)}
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
      onclick={() => onchange(preset.id)}
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
