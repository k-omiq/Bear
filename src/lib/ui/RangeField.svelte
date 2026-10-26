<script>
  import { untrack } from 'svelte'

  /**
   * A number with three ways to set it: a slider to sweep, a box to type the
   * exact value into, and steps - the arrow keys on either, and the wheel over
   * the whole control. Brush size and mask padding are both this, wherever they
   * are drawn, so the two cannot drift into three different feels again.
   *
   * **A log track** (`scale: 'log'`) gives a doubling the same distance at 4 px
   * as at 200. A brush is chosen by feel at the small end and by eye at the
   * large one, and a linear track spent most of its length on sizes nobody
   * fine-tunes.
   *
   * **Two callbacks.** `oninput` follows the value while it moves - the thumb
   * dragged, a run of arrow presses or wheel ticks - and `onchange` is the value
   * it settles on: the thumb let go, Enter or a blur in the box, and a run of
   * steps once it stops for `SETTLE_MS`. A brush size is cheap and passes the
   * same function to both; a mask padding is a save and an undo step, so it
   * listens to `onchange` alone and a held arrow key saves once.
   *
   * `stepper` is the step a press takes, from a value and a direction; one more
   * than `step` by default. A size passes `gesture.js#steppedSize`, so an arrow
   * here and `]` on the canvas move the brush the same amount.
   *
   * The box needs a name of its own (`boxLabel`, translated by the caller):
   * two controls answering to one name is one too many for a screen reader.
   *
   * **One bar, not three parts.** The control is drawn as a single bar that
   * fills from the left as the value grows, with the name inside it on the
   * left and the number on the right. The slider is the whole bar, invisible
   * over it, so a press anywhere sets the value and a drag scrubs it; the
   * number is a box of its own above the slider, so a press on it types.
   * A slider, a separate box and a label each in its own style read as three
   * controls for one number.
   *
   * `compact` is the tool bar's fixed width; `row` and `stacked` fill the
   * width they are given, in a panel, a popover, a menu or a list row.
   * In a menu or a list (`listItem`), Up and Down belong to the list, which
   * moves between its items with them, and only Left and Right step the value.
   * In something that scrolls (`wheel: 'focused'`), the wheel steps the value
   * only once the control has the focus, so scrolling the list past it does
   * not change it.
   *
   * @type {{
   *   id?: string,
   *   label: string,
   *   boxLabel?: string,
   *   value: number,
   *   min: number,
   *   max: number,
   *   step?: number,
   *   unit?: string,
   *   scale?: 'linear'|'log',
   *   stepper?: (value: number, direction: number) => number,
   *   layout?: 'row'|'compact'|'stacked',
   *   hint?: string,
   *   listItem?: boolean,
   *   wheel?: 'always'|'focused',
   *   disabled?: boolean,
   *   oninput?: (value: number) => void,
   *   onchange: (value: number) => void,
   * }}
   */
  let {
    id: customId,
    label,
    boxLabel,
    value,
    min,
    max,
    step = 1,
    unit = '',
    scale = 'linear',
    stepper,
    layout = 'row',
    hint,
    listItem = false,
    wheel = 'always',
    disabled = false,
    oninput,
    onchange,
  } = $props()

  /** How long a run of steps waits, after the last one, before it settles. */
  const SETTLE_MS = 250
  /** Positions on a log track. Fine enough that every pixel of 1-500 has one. */
  const SPAN = 4000

  const autoId = $props.id()
  const id = $derived(customId ?? autoId)
  const log = $derived(scale === 'log' && min > 0)

  /** The value while it moves, before it settles; null when it is the prop's. */
  let draft = $state(/** @type {number|null} */ (null))
  /** What the box holds while it is being typed in; null when it shows the value. */
  let typed = $state(/** @type {string|null} */ (null))
  /** @type {ReturnType<typeof setTimeout>|null} */
  let settleTimer = null
  /** @type {HTMLElement|undefined} */
  let root = $state()
  /** @type {HTMLInputElement|undefined} */
  let track = $state()

  /**
   * Focus the slider: a menu moves its focus between items, and this is one.
   *
   * @param {FocusOptions} [options]
   */
  export function focus(options) {
    track?.focus(options)
  }

  /**
   * Settle now whatever is waiting - a typed value, a run of steps - so a
   * caller about to act on the value (a menu entry) sees it.
   */
  export function flush() {
    if (typed !== null) commitTyped()
    else if (draft !== null) settle(draft)
  }

  const shown = $derived(draft ?? value)
  const valueText = $derived(`${shown}${unit ? ` ${unit.trim()}` : ''}`)
  const title = $derived(hint ? `${label} · ${hint}` : label)

  /** @param {number} v */
  function clampRound(v) {
    const stepped = Math.round((v - min) / step) * step + min
    return Math.min(max, Math.max(min, Number(stepped.toFixed(6))))
  }

  /** @param {number} v */
  function toPosition(v) {
    if (!log) return v
    return Math.round(((Math.log(v) - Math.log(min)) / (Math.log(max) - Math.log(min))) * SPAN)
  }

  /** @param {number} position */
  function fromPosition(position) {
    if (!log) return clampRound(position)
    return clampRound(Math.exp(Math.log(min) + (position / SPAN) * (Math.log(max) - Math.log(min))))
  }

  /** @param {number} next */
  function move(next) {
    // A drag that takes over from a run of steps settles on its release, not
    // in the middle of it.
    if (settleTimer) clearTimeout(settleTimer)
    settleTimer = null
    if (next === shown) return
    draft = next
    oninput?.(next)
  }

  /**
   * End a move: `onchange` hears it when the value moved, and when a draft
   * ends even on the value the parent already shows, since a parent that
   * follows `oninput` live still waits for this to commit it.
   *
   * @param {number} next
   */
  function settle(next) {
    if (settleTimer) clearTimeout(settleTimer)
    settleTimer = null
    const moved = draft !== null
    draft = null
    if (moved || next !== value) onchange(next)
  }

  /** @param {number} direction */
  function stepBy(direction) {
    if (disabled) return
    const from = shown
    const next = clampRound(stepper ? stepper(from, direction) : from + Math.sign(direction) * step)
    move(next)
    if (settleTimer) clearTimeout(settleTimer)
    settleTimer = setTimeout(() => settle(draft ?? value), SETTLE_MS)
  }

  /**
   * The slider's own keys, taken over so a press is one `stepper` step on
   * either track - the browser's would be a thousandth of a log track - and
   * so Home and End reach the ends of the real range.
   *
   * @param {KeyboardEvent} event
   */
  function onTrackKey(event) {
    if (event.metaKey || event.ctrlKey || event.altKey) return
    if (listItem && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) return
    const big = event.shiftKey ? 5 : 1
    /** @type {Record<string, () => void>} */
    const keys = {
      ArrowUp: () => { for (let i = 0; i < big; i += 1) stepBy(1) },
      ArrowRight: () => { for (let i = 0; i < big; i += 1) stepBy(1) },
      ArrowDown: () => { for (let i = 0; i < big; i += 1) stepBy(-1) },
      ArrowLeft: () => { for (let i = 0; i < big; i += 1) stepBy(-1) },
      PageUp: () => { for (let i = 0; i < 5; i += 1) stepBy(1) },
      PageDown: () => { for (let i = 0; i < 5; i += 1) stepBy(-1) },
      Home: () => settle(min),
      End: () => settle(max),
    }
    const run = keys[event.key]
    if (!run) return
    event.preventDefault()
    run()
  }

  /** @param {KeyboardEvent} event */
  function onBoxKey(event) {
    if (event.key === 'Enter') {
      event.preventDefault()
      commitTyped()
    } else if (event.key === 'Escape') {
      // Put the value back; the shell's Escape then takes the focus out.
      typed = null
    } else if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
      if (listItem) return
      event.preventDefault()
      // A typed value is where the step starts: 50, then Up, is 51.
      if (typed !== null) {
        const parsed = Number.parseFloat(typed.replace(',', '.'))
        if (Number.isFinite(parsed)) move(clampRound(parsed))
        typed = null
      }
      stepBy(event.key === 'ArrowUp' ? 1 : -1)
    }
  }

  function commitTyped() {
    if (typed === null) return
    const parsed = Number.parseFloat(typed.replace(',', '.'))
    typed = null
    if (Number.isFinite(parsed)) settle(clampRound(parsed))
  }

  // The wheel over any part of the control steps it, without scrolling what
  // is behind it. Added by hand because it has to be able to cancel the
  // scroll, which a passive listener cannot. A mouse notch is one step,
  // however few pixels the platform says it is; a trackpad sends many small
  // deltas, so those are summed to a step.
  $effect(() => {
    const node = root
    if (!node) return
    let summed = 0
    /** @param {WheelEvent} event */
    const onwheel = (event) => {
      if (disabled || event.ctrlKey || event.deltaY === 0) return
      if (wheel === 'focused' && !node.contains(document.activeElement)) return
      event.preventDefault()
      const legacy = /** @type {any} */ (event).wheelDeltaY
      const notch = event.deltaMode !== 0 || (typeof legacy === 'number' && legacy !== 0 && legacy % 120 === 0)
      if (notch) {
        summed = 0
        stepBy(event.deltaY < 0 ? 1 : -1)
        return
      }
      summed += event.deltaY
      while (Math.abs(summed) >= 50) {
        stepBy(summed < 0 ? 1 : -1)
        summed -= Math.sign(summed) * 50
      }
    }
    node.addEventListener('wheel', onwheel, { passive: false })
    return () => node.removeEventListener('wheel', onwheel)
  })

  // A value changed from outside ends what the control was holding: the box's
  // typing, unless the box is being typed in, and a run of steps. The newer
  // change wins: a tool switch keeps this control for the next tool's Size,
  // and a preset or a key that moved the value must not be undone by a run
  // of steps settling after it.
  let lastValue = untrack(() => value)
  $effect.pre(() => {
    const now = value
    if (now === lastValue) return
    lastValue = now
    untrack(() => {
      if (!(document.activeElement instanceof HTMLInputElement && document.activeElement.classList.contains('number')
        && root?.contains(document.activeElement))) typed = null
      if (draft !== null && draft !== now) {
        if (settleTimer) clearTimeout(settleTimer)
        settleTimer = null
        draft = null
      }
    })
  })

  // A run of steps still waiting to settle is settled as the control goes: a
  // menu closed straight after three presses keeps the value it showed.
  $effect(() => () => {
    if (settleTimer) clearTimeout(settleTimer)
    settleTimer = null
    if (typed !== null) commitTyped()
    else if (draft !== null) onchange(draft)
  })
</script>

<div
  bind:this={root}
  class="field {layout}"
  class:disabled
  data-range-field
  style:--fill="{log ? (toPosition(shown) / SPAN) * 100 : ((shown - min) / (max - min || 1)) * 100}%"
  {title}
>
  <span class="fill" aria-hidden="true"></span>
  <label class="label" for={id}>{label}</label>
  <input
    bind:this={track}
    {id}
    class="track"
    type="range"
    min={log ? 0 : min}
    max={log ? SPAN : max}
    step={log ? 1 : step}
    value={toPosition(shown)}
    {disabled}
    aria-valuemin={min}
    aria-valuemax={max}
    aria-valuenow={shown}
    aria-valuetext={valueText}
    oninput={(e) => move(fromPosition(Number(e.currentTarget.value)))}
    onchange={(e) => settle(fromPosition(Number(e.currentTarget.value)))}
    onkeydown={onTrackKey}
  />
  <span class="box">
    <input
      class="number"
      type="text"
      inputmode="decimal"
      autocomplete="off"
      spellcheck="false"
      size="3"
      value={typed ?? String(shown)}
      {disabled}
      aria-label={boxLabel ?? label}
      oninput={(e) => (typed = e.currentTarget.value)}
      onfocus={(e) => {
        // A run of steps still waiting settles first, so it cannot land on
        // top of what is typed next.
        if (settleTimer) settle(draft ?? value)
        e.currentTarget.select()
      }}
      onblur={commitTyped}
      onkeydown={onBoxKey}
    />
    {#if unit}<span class="unit" aria-hidden="true">{unit.trim()}</span>{/if}
  </span>
</div>

<style>
  /* The bar. Its fill is the value: the tint of the theme's accent, with the
     accent itself as a hairline at the end, which is where the thumb would
     be. Layers, back to front: the fill, the name, the invisible slider over
     the whole bar, and the number above the slider so it can be typed in. */
  .field {
    position: relative;
    display: flex;
    align-items: center;
    width: 100%;
    min-width: 0;
    height: 26px;
    border-radius: var(--r-chip);
    background: var(--accent-soft);
    overflow: hidden;
    isolation: isolate;
    transition: box-shadow var(--dur-fast) var(--ease);
  }
  .field.compact { flex: none; width: 156px }

  .fill {
    position: absolute;
    inset: 0 auto 0 0;
    /* Never narrower than the hairline: an empty bar still shows where its
       value is, and that it is something to drag. */
    width: max(2px, var(--fill));
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    box-shadow: inset -2px 0 0 var(--accent);
    pointer-events: none;
    transition: background var(--dur-fast) var(--ease);
  }
  .field:hover .fill { background: color-mix(in srgb, var(--accent) 22%, transparent) }
  .field:has(.track:active) .fill { background: color-mix(in srgb, var(--accent) 26%, transparent) }

  .label {
    position: relative;
    z-index: 1;
    flex: 1;
    min-width: 0;
    padding-left: 9px;
    overflow: hidden;
    font-size: 11px;
    color: var(--text);
    white-space: nowrap;
    text-overflow: ellipsis;
    pointer-events: none;
  }

  .track {
    position: absolute;
    inset: 0;
    z-index: 2;
    width: 100%;
    height: 100%;
    margin: 0;
    opacity: 0;
    cursor: ew-resize;
    -webkit-appearance: none;
    appearance: none;
  }
  /* A thumb as thin as the hairline, so a press lands the value under the
     pointer rather than half a thumb away from it. */
  /* WebKit hit-tests a press against the runnable track, which is a few
     pixels tall unless it is told otherwise. */
  .track::-webkit-slider-runnable-track { height: 100% }
  .track::-webkit-slider-thumb { -webkit-appearance: none; width: 2px; height: 26px }
  .track::-moz-range-thumb { width: 2px; height: 26px; border: 0 }

  .field:has(.track:focus-visible) { box-shadow: 0 0 0 2px var(--tintline) }

  .box {
    position: relative;
    z-index: 3;
    display: inline-flex;
    flex: none;
    align-items: baseline;
    gap: 2px;
    height: 20px;
    margin-right: 3px;
    padding: 0 6px 0 2px;
    border-radius: calc(var(--r-chip) - 2px);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    line-height: 20px;
    cursor: text;
    transition: background var(--dur-fast) var(--ease);
  }
  .box:hover { background: color-mix(in srgb, var(--panel) 70%, transparent) }
  .box:focus-within {
    background: var(--panel);
    box-shadow: 0 0 0 1px var(--tintline);
  }
  .number {
    width: 3.2ch;
    min-width: 0;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--text);
    font: inherit;
    font-weight: 600;
    text-align: right;
    outline: none;
  }
  .unit { color: var(--t3) }

  .disabled { opacity: .45 }
  .disabled .track { cursor: default }
  .disabled .box { cursor: default }
</style>
