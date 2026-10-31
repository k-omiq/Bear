<script>
  import Icon from '../icons/Icon.svelte'

  /**
   * Square icon button. 30x30 everywhere except the editor's tool rail, which
   * is 34. `label` is required: it is the accessible name and, joined with the
   * shortcut as `Label - K`, the tooltip.
   *
   * `active` is purely visual (accent fill). Toggles should also pass
   * `pressed={active}` so the state reaches assistive tech. Radio-like rails
   * (the tool stack) instead pass `role="radio" aria-checked={…} tabindex={…}`
   * through the rest props and own the roving tabindex themselves.
   *
   * @type {{
   *   icon: string,
   *   label: string,
   *   shortcut?: string,
   *   keyshortcuts?: string,
   *   active?: boolean,
   *   pressed?: boolean,
   *   disabled?: boolean,
   *   size?: number,
   *   iconSize?: number,
   *   onclick?: (e: MouseEvent) => void,
   *   [key: string]: unknown,
   * }}
   */
  let {
    icon,
    label,
    shortcut,
    keyshortcuts,
    active = false,
    pressed,
    disabled = false,
    size = 30,
    iconSize,
    onclick,
    ...rest
  } = $props()

  const tooltip = $derived(shortcut ? `${label} · ${shortcut}` : label)
  const glyphSize = $derived(iconSize ?? (size >= 34 ? 17 : size <= 24 ? 13 : 16))
</script>

<button
  type="button"
  class="ibtn"
  class:active
  class:disabled
  {disabled}
  {onclick}
  style:width="{size}px"
  style:height="{size}px"
  title={tooltip}
  aria-label={label}
  aria-keyshortcuts={keyshortcuts || undefined}
  aria-pressed={pressed}
  {...rest}
>
  <Icon name={icon} size={glyphSize} />
</button>

<style>
  .ibtn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: none;
    padding: 0;
    border: 1px solid transparent;
    border-radius: var(--r-lg);
    background: transparent;
    color: var(--t2);
    cursor: pointer;
    transition:
      background var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .ibtn:hover:not(:disabled) { background: var(--accent-soft); color: var(--text) }

  .active { background: var(--accent); color: var(--accent-fg) }
  .active:hover:not(:disabled) { background: var(--accent); color: var(--accent-fg) }

  .ibtn:disabled { opacity: .35; cursor: default }
</style>
