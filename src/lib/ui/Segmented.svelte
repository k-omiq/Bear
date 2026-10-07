<script>
  import Icon from '../icons/Icon.svelte'

  /**
   * A real radio group rendered as the prototype's chips. Used for tool
   * parameters (right-aligned, wraps) and for Settings / Export rows.
   *
   * Selection follows focus, as radio groups do: arrow keys move and select,
   * Home / End jump to the ends, disabled options are skipped, and the group
   * holds exactly one tab stop (roving tabindex).
   *
   * Options are strings or `{ value, label, disabled?, title?, icon?, ariaLabel? }`.
   *
   * An option with an `icon` draws that glyph in a square cell and no text at
   * all; its `label` becomes the cell's accessible name and its tooltip. This
   * is the tool bar's shape - four shapes, page against project, aligned
   * against non-aligned - where the alternatives are things with pictures and the bar has no
   * room for their words. Mixing the two in one group is not offered: a row of
   * cells where some are square glyphs and some are words has no rhythm.
   *
   * `ariaLabel` is the other half of the same problem, for the option whose
   * label is a *glyph typed as text* - a keycap such as `⌥` - where there is
   * no icon to name and the visible character is unreadable aloud. It wins
   * over the name an `icon` would have derived from `label`.
   *
   * `block` is a panel row's shape: the group fills its column and the options
   * divide it into equal cells, so two rows of chips line up with each other
   * and with the sliders above them instead of each ending wherever its own
   * words end. It is for **short, closed** lists - past three options the cells
   * are too narrow for their labels and `Select` is the control (see its own
   * note).
   *
   * `describedBy` names an element that says something about the group as a
   * whole - in practice the one sentence a caller owes when it has disabled an
   * option, since a `title` on a disabled button reaches nobody. The group
   * carries the association because a `role="radiogroup"` has no room inside
   * it for a paragraph.
   *
   * @type {{
   *   options: Array<string | {
   *     value: string, label?: string, disabled?: boolean, title?: string,
   *     icon?: string, ariaLabel?: string,
   *   }>,
   *   value: string,
   *   onchange: (value: string) => void,
   *   size?: 'chip' | 'md',
   *   align?: 'start' | 'end',
   *   block?: boolean,
   *   label?: string,
   *   labelledBy?: string,
   *   describedBy?: string,
   *   disabled?: boolean,
   * }}
   */
  let {
    options,
    value,
    onchange,
    size = 'chip',
    align = 'end',
    block = false,
    label,
    labelledBy,
    describedBy,
    disabled = false,
  } = $props()

  const items = $derived(
    options.map((o) =>
      typeof o === 'string'
        ? { value: o, label: o, disabled: false, title: undefined, icon: undefined, ariaLabel: undefined }
        : {
            value: o.value,
            label: o.label ?? o.value,
            disabled: !!o.disabled,
            title: o.title,
            icon: o.icon,
            ariaLabel: o.ariaLabel,
          },
    ),
  )

  const selected = $derived(items.findIndex((o) => o.value === value))

  // Exactly one tab stop: the selected option if enabled, or the first enabled
  // one when nothing is selected or the selection is disabled.
  const tabIndexOf = $derived.by(() => {
    const stop = disabled
      ? -1
      : selected >= 0 && !items[selected]?.disabled
        ? selected
        : items.findIndex((o) => !o.disabled)
    return (/** @type {number} */ i) => (i === stop ? 0 : -1)
  })

  /** @type {HTMLButtonElement[]} */
  let buttons = $state([])

  $effect(() => {
    if (buttons.length > items.length) buttons.length = items.length
  })

  /**
   * @param {number} from
   * @param {number} step
   */
  function move(from, step) {
    const n = items.length
    if (!n) return
    for (let k = 1; k <= n; k++) {
      const i = (((from + step * k) % n) + n) % n
      if (!items[i].disabled) {
        onchange(items[i].value)
        buttons[i]?.focus()
        return
      }
    }
  }

  /**
   * @param {number} step
   */
  function edge(step) {
    const start = step > 0 ? -1 : items.length
    move(start, step)
  }

  /**
   * @param {KeyboardEvent} e
   * @param {number} i
   */
  function onkeydown(e, i) {
    if (disabled) return
    switch (e.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        claim(e); move(i, 1); break
      case 'ArrowLeft':
      case 'ArrowUp':
        claim(e); move(i, -1); break
      case 'Home':
        claim(e); edge(1); break
      case 'End':
        claim(e); edge(-1); break
      default:
    }
  }

  /**
   * The group has answered this key, so nothing else may. Without the
   * `stopPropagation` the shortcut layer on `window` sees the same arrow and
   * pages the chapter as well as moving the selection.
   *
   * @param {KeyboardEvent} e
   */
  function claim(e) {
    e.preventDefault()
    e.stopPropagation()
  }
</script>

<div
  class="seg {size}"
  class:end={align === 'end' && !block}
  class:block
  role="radiogroup"
  aria-label={labelledBy ? undefined : label}
  aria-labelledby={labelledBy}
  aria-describedby={describedBy || undefined}
  aria-disabled={disabled || undefined}
>
  {#each items as o, i (o.value)}
    <button
      bind:this={buttons[i]}
      type="button"
      role="radio"
      class="opt"
      class:on={o.value === value}
      class:glyph={!!o.icon}
      aria-checked={o.value === value}
      tabindex={tabIndexOf(i)}
      disabled={disabled || o.disabled}
      title={o.title ?? (o.icon ? o.label : undefined)}
      aria-label={o.ariaLabel ?? (o.icon ? o.label : undefined)}
      onclick={() => onchange(o.value)}
      onkeydown={(e) => onkeydown(e, i)}
    >{#if o.icon}<Icon name={o.icon} size={15} />{:else}{o.label}{/if}</button>
  {/each}
</div>

<style>
  .seg {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--s-1) 5px;
  }
  .seg.end { justify-content: flex-end }

  /* One row, equal cells, the column's whole width. `minmax(0, 1fr)` rather
     than `1fr` so a long label ellipsises inside its cell instead of pushing
     the cell wider than its share. */
  .seg.block {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: minmax(0, 1fr);
    width: 100%;
    gap: 4px;
  }
  .seg.block .opt {
    min-width: 0;
    padding: 0 var(--s-1);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .opt {
    border: 1px solid transparent;
    white-space: nowrap;
    cursor: pointer;
    background: var(--accent-soft);
    transition:
      background var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }

  /* The metrics table says `--t3` for an idle chip. It measures ≈3.3:1 against
     `--accent-soft` in light, and this is an interactive label rather than
     decoration, so it is `--t2` - the one deliberate departure from the table. */
  .chip .opt { height: 23px; padding: 0 9px; border-radius: var(--r-chip); font-size: 11px; color: var(--t2) }
  .md .opt   { height: 26px; padding: 0 10px; border-radius: var(--r-sm);  font-size: 12px; color: var(--t2) }

  /* Square, and larger than a text chip is tall: a glyph has no descenders to
     borrow height from, and 28 is the size the bar's other controls are. The
     size class's padding and font are overridden rather than avoided, so a
     caller can keep passing `size` without knowing which kind of cell it gets. */
  .opt.glyph {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border-radius: var(--r-chip);
  }

  .opt:hover:not(:disabled) { color: var(--text) }

  .opt.on {
    background: var(--accent);
    color: var(--accent-fg);
    font-weight: 600;
  }
  .opt.on:hover:not(:disabled) { color: var(--accent-fg) }

  /* A dashed hairline as well as the dimming: "not available" reads as a
     change of shape, not only of weight, so the state does not rest on
     contrast alone. Whoever disables an option owes the user the reason in
     text as well - see `editor/ToolBar.svelte`'s gate note. */
  .opt:disabled {
    opacity: .38;
    border-style: dashed;
    border-color: var(--line2);
    cursor: default;
  }
</style>
