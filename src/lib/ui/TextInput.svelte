<script>
  import Icon from '../icons/Icon.svelte'

  /**
   * Single-line text field. The only text-entry primitive in the app: the
   * New project / New chapter names, the source path, the Open project
   * dialog's filter and the editor's project name.
   *
   * Metrics follow the prototype's field: h 32 / radius 7 / bg `--panel2` /
   * pad 0 11 / font 12.5 for the dialog size, and a 26px `sm` that lines up
   * with `Button` in a toolbar row. A `--line` hairline is added because the
   * field can sit on `--bg`, where `--panel2` alone is nearly invisible.
   *
   * Controlled, like `Segmented` and `Slider`: the caller owns `value` and
   * gets every keystroke through `onchange`.
   *
   * `numeric` makes it a digit-only field. It stays `type="text"` on purpose:
   * `type="number"` accepts `e`, `+`, `-` and `.` as valid characters and
   * reports them as an empty `value`, so a field that must hold a chapter
   * number would silently read as blank. Digits are filtered on the way in
   * instead, which also covers a paste, and `inputmode` still brings up the
   * numeric keypad.
   *
   * `reject` replaces the digit filter with a caller's own: a pattern of the
   * characters to drop, global (`/[^0-9.,/-]/g` for a split chapter number).
   *
   * @type {{
   *   value: string,
   *   onchange: (value: string) => void,
   *   numeric?: boolean,
   *   reject?: RegExp,
   *   placeholder?: string,
   *   label?: string,
   *   id?: string,
   *   icon?: string,
   *   size?: 'md' | 'sm',
   *   disabled?: boolean,
   *   onkeydown?: (e: KeyboardEvent) => void,
   *   [key: string]: unknown,
   * }}
   */
  let {
    value,
    onchange,
    numeric = false,
    reject,
    placeholder,
    label,
    id,
    icon,
    size = 'md',
    disabled = false,
    onkeydown,
    ...rest
  } = $props()

  /** @type {HTMLInputElement | undefined} */
  let input = $state()

  /** Focus the field. Exported for `bind:this` - `editor/ProjectName.svelte`
   * moves focus here the moment the name becomes editable. */
  export function focus() {
    input?.focus()
  }

  /**
   * A digit-only field rewrites its own element when a non-digit arrives, so
   * the rejected character never appears - the caller is controlled, and
   * Svelte would not re-render an `input` whose bound `value` did not change.
   *
   * @param {Event & {currentTarget: HTMLInputElement}} event
   */
  function oninput(event) {
    const raw = event.currentTarget.value
    if (!numeric && !reject) {
      onchange(raw)
      return
    }
    const digits = raw.replace(reject ?? /[^0-9]/g, '')
    if (digits !== raw) {
      const caret = event.currentTarget.selectionStart ?? digits.length
      event.currentTarget.value = digits
      const back = raw.length - digits.length
      event.currentTarget.setSelectionRange?.(Math.max(0, caret - back), Math.max(0, caret - back))
    }
    onchange(digits)
  }
</script>

<div class="wrap {size}" class:disabled>
  {#if icon}<span class="lead"><Icon name={icon} size={13} /></span>{/if}
  <input
    bind:this={input}
    {id}
    {placeholder}
    {disabled}
    {onkeydown}
    type="text"
    inputmode={numeric ? 'numeric' : undefined}
    class="input"
    aria-label={label}
    value={value}
    oninput={oninput}
    {...rest}
  />
</div>

<style>
  /* `min-width: 0` so the field can be shrunk by a row that has other things
     in it. Without it a flex item's automatic minimum is its content's, and
     the tool window's colour row - swatch, this field, eyedropper - pushed the
     eyedropper out past the panel edge at narrow widths, where the window's
     own `overflow: hidden` clipped it away entirely. */
  .wrap {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    width: 100%;
    min-width: 0;
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--panel2);
    transition: border-color var(--dur-fast) var(--ease);
  }
  .wrap:hover:not(.disabled) { border-color: var(--line2) }
  .wrap:focus-within { border-color: var(--line2) }
  .disabled { opacity: .5 }

  .md { height: 32px; padding: 0 11px }
  .sm { height: 26px; padding: 0 9px }

  .lead { display: flex; color: var(--t3) }

  .input {
    flex: 1;
    min-width: 0;
    height: 100%;
    border: none;
    background: transparent;
    outline: none;
  }
  .md .input { font-size: 12.5px }
  .sm .input { font-size: 12px }
  .input::placeholder { color: var(--t3) }
</style>
