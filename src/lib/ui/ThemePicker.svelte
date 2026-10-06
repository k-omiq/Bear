<script>
  /**
   * A radio group of theme swatches.
   *
   * Each swatch wears its own theme through a local `data-theme` attribute, so
   * it is drawn from the same token blocks in `app.css` that paint the app,
   * and cannot drift from them. `system` is drawn split, light over dark.
   *
   * Keys behave as in `Segmented`: arrows move and select, Home / End jump to
   * the ends, and the group holds one tab stop.
   *
   * @type {{
   *   options: Array<{ value: string, label: string }>,
   *   value: string,
   *   onchange: (value: string) => void,
   *   label?: string,
   *   labelledBy?: string,
   * }}
   */
  let { options, value, onchange, label, labelledBy } = $props()

  /** @type {HTMLButtonElement[]} */
  let buttons = $state([])

  /** @param {KeyboardEvent} event @param {number} index */
  function onkeydown(event, index) {
    const last = options.length - 1
    let next
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        next = index === last ? 0 : index + 1
        break
      case 'ArrowLeft':
      case 'ArrowUp':
        next = index === 0 ? last : index - 1
        break
      case 'Home':
        next = 0
        break
      case 'End':
        next = last
        break
      default:
        return
    }
    event.preventDefault()
    event.stopPropagation()
    onchange(options[next].value)
    buttons[next]?.focus()
  }
</script>

<div class="picker" role="radiogroup" aria-label={label} aria-labelledby={labelledBy}>
  {#each options as option, index (option.value)}
    {@const on = option.value === value}
    <button
      bind:this={buttons[index]}
      type="button"
      role="radio"
      class="option"
      class:on
      aria-checked={on}
      tabindex={on ? 0 : -1}
      onclick={() => onchange(option.value)}
      onkeydown={(event) => onkeydown(event, index)}
    >
      <span class="swatch" aria-hidden="true">
        {#if option.value === 'system'}
          <span class="half" data-theme="light"><span class="card"><i></i><b></b></span></span>
          <span class="half" data-theme="dark"><span class="card"><i></i><b></b></span></span>
        {:else}
          <span class="half" data-theme={option.value}><span class="card"><i></i><b></b></span></span>
        {/if}
      </span>
      <span class="name">{option.label}</span>
    </button>
  {/each}
</div>

<style>
  .picker {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: var(--s-4);
  }

  .option {
    display: flex;
    flex-direction: column;
    gap: var(--s-3);
    padding: 0;
    border: none;
    background: none;
    color: var(--t2);
    font-size: 11.5px;
    text-align: left;
    cursor: pointer;
  }
  .option:hover, .option.on { color: var(--text) }
  .option.on .name { font-weight: 600 }

  .swatch {
    display: flex;
    flex-direction: column;
    height: 84px;
    overflow: hidden;
    border-radius: var(--r-lg);
    box-shadow: 0 0 0 1px var(--line2);
    transition: box-shadow var(--dur-fast) var(--ease), transform var(--dur-fast) var(--ease);
  }
  .option:hover .swatch { box-shadow: 0 0 0 1px var(--tintline) }
  .option.on .swatch { box-shadow: 0 0 0 2px var(--accent) }
  .option:active .swatch { transform: scale(.98) }
  .option:focus-visible { outline: none }
  .option:focus-visible .swatch { outline: 2px solid var(--accent); outline-offset: 3px }

  /* One theme's field. `system` stacks two of them. */
  .half {
    flex: 1;
    display: flex;
    align-items: flex-end;
    padding: 8px 0 0 12px;
    background: var(--bg);
    overflow: hidden;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 5px;
    width: 100%;
    height: 100%;
    min-height: 26px;
    padding: 8px 9px;
    border-radius: var(--r-sm) 0 0 0;
    background: var(--panel);
    box-shadow: var(--edge-soft);
  }
  .card i, .card b { display: block; height: 4px; border-radius: var(--r-pill) }
  .card i { width: 60%; background: var(--t3) }
  .card b { width: 34%; background: var(--accent) }
  .half + .half .card { min-height: 0 }

  .name { padding-left: 1px }
</style>
