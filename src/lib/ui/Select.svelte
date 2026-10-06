<script>
  import Icon from '../icons/Icon.svelte'

  /**
   * A one-of-many picker for lists too long to be chips.
   *
   * `Segmented` is the app's radio group and stays the right control while a
   * row holds two or three options: every option is visible, and the whole row
   * is one press away. Past three it stops being that - the chips wrap onto a
   * second and third line, the row grows taller than the slider above it, and
   * the engine ladder is five names long and will not get shorter. This is
   * that row instead: one line, one width, whatever the list holds.
   *
   * It is a **native `<select>`** under a painted chevron, which is the same
   * control a Layers row's engine picker already is. The popup a native select
   * opens is drawn by the platform rather than by the page, so it is the one
   * kind of menu that can be opened from inside a floating window without
   * being clipped by the window's own `overflow: hidden` - and it arrives with
   * type-ahead, `Home`/`End`, and the platform's own touch and screen-reader
   * behaviour already in it.
   *
   * Controlled, like `Segmented` and `Slider`: the caller owns `value` and
   * gets every change through `onchange`.
   *
   * Options are `{ value, label?, disabled?, title? }`.
   *
   * **Two sizes, one control.** `md` is the tool window's - 26 high, the
   * metrics of an `md` chip, so a row that is a select and a row that is a
   * group of chips are the same height. `row` is the Layers panel's, 22 high
   * and a half-point smaller, which is the scale that list is drawn at. They
   * exist because a Layers row used to draw a `<select>` of its own with its
   * own border and its own font size: the same
   * control asking the same question in two appearances.
   *
   * **`fit`** takes the control down to the width of its own longest option
   * rather than its container's. It is the tool window's, where a choice row
   * stacks its control under its label and "everything to the right" is the
   * whole panel: a 280px bar reading *Rect* was the largest single piece of
   * empty space left in it. A native `<select>` measures its widest option
   * for us, so the control is exactly as wide as the longest thing it will
   * ever have to show - in whatever language it is showing it - and the chips
   * beside it in the same panel are their own width for the same reason.
   *
   * @type {{
   *   options: Array<{ value: string, label?: string, disabled?: boolean, title?: string }>,
   *   value: string,
   *   onchange: (value: string) => void,
   *   size?: 'md' | 'row',
   *   fit?: boolean,
   *   label?: string,
   *   labelledBy?: string,
   *   describedBy?: string,
   *   id?: string,
   *   title?: string,
   *   disabled?: boolean,
   * }}
   */
  let {
    options,
    value,
    onchange,
    size = 'md',
    fit = false,
    label,
    labelledBy,
    describedBy,
    id,
    title,
    disabled = false,
  } = $props()

  /** @param {Event & { currentTarget: HTMLSelectElement }} event */
  function pick(event) {
    const next = event.currentTarget.value
    if (next !== value) onchange(next)
  }
</script>

<div class="wrap {size}" class:fit class:disabled>
  <select
    {id}
    {value}
    {disabled}
    {title}
    class="select"
    aria-label={labelledBy ? undefined : label}
    aria-labelledby={labelledBy}
    aria-describedby={describedBy}
    onchange={pick}
  >
    {#each options as option (option.value)}
      <option value={option.value} disabled={option.disabled} title={option.title}>
        {option.label ?? option.value}
      </option>
    {/each}
  </select>
  <span class="chev" aria-hidden="true"><Icon name="chevron-down" size={12} /></span>
</div>

<style>
  .wrap {
    position: relative;
    display: flex;
    align-items: center;
    width: 100%;
    min-width: 0;
  }
  /* As wide as its own longest option, not as wide as what is around it. The
     floor is on the control rather than on the wrapper, because the wrapper is
     what the chevron is positioned against: a wrapper wider than the select
     inside it puts the chevron past the control's own edge. The floor itself
     keeps a list of four short words from coming out as a stub beside the
     chips it shares a panel with. */
  .wrap.fit { width: auto; max-width: 100% }
  .fit .select { width: auto; min-width: 96px; max-width: 100% }
  .disabled { opacity: .45 }

  /* The metrics of a `md` chip - 26 high, the segmented radius - so a row that
     is a select and a row that is a group of chips are the same height. The
     `row` size is the Layers panel's own line height instead. */
  .select {
    -webkit-appearance: none;
    -moz-appearance: none;
    appearance: none;
    width: 100%;
    min-width: 0;
    border: 1px solid var(--line2);
    border-radius: var(--r-chip);
    background: var(--accent-soft);
    color: var(--t2);
    text-overflow: ellipsis;
    cursor: pointer;
    transition:
      background var(--dur-fast) var(--ease),
      border-color var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .md .select  { height: 26px; padding: 0 24px 0 var(--s-3); font-size: 11px }
  .row .select { height: 22px; padding: 0 22px 0 var(--s-2); font-size: 10.5px }

  .select:hover:not(:disabled) { color: var(--text); border-color: var(--tint) }
  .select:disabled { cursor: default }

  /* The chevron is decoration over the control and must never eat the press
     that opens it. */
  .chev {
    position: absolute;
    right: var(--s-2);
    display: flex;
    color: var(--t3);
    pointer-events: none;
  }
</style>
