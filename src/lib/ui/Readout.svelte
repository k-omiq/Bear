<script>
  /**
   * A number that changes, in a bar: the zoom percentage, the page position,
   * the `n / total` of the review set.
   *
   * The visible text is the bare figure (`3 / 24`, `100%`) - locale-neutral,
   * tabular, and never a translated sentence. The *accessible* name is the
   * translated sentence, so assistive tech says "Page 3 of 24" where the eye
   * reads "3 / 24". `text` is therefore hidden from the accessibility tree
   * when the readout is static.
   *
   * With `onclick` it is a real button (the zoom readout snaps to 100%);
   * without, it is inert text.
   *
   * @type {{
   *   text: string,
   *   label: string,
   *   shortcut?: string,
   *   keyshortcuts?: string,
   *   minWidth?: number,
   *   onclick?: (e: MouseEvent) => void,
   *   [key: string]: unknown,
   * }}
   */
  let { text, label, shortcut, keyshortcuts, minWidth = 44, onclick, ...rest } = $props()

  const tooltip = $derived(shortcut ? `${label} · ${shortcut}` : label)
</script>

{#if onclick}
  <button
    type="button"
    class="readout button"
    style:min-width="{minWidth}px"
    title={tooltip}
    aria-label={label}
    aria-keyshortcuts={keyshortcuts || undefined}
    {onclick}
    {...rest}
  >
    {text}
  </button>
{:else}
  <span class="readout" style:min-width="{minWidth}px" title={tooltip} {...rest}>
    <span aria-hidden="true">{text}</span>
    <span class="sr">{label}</span>
  </span>
{/if}

<style>
  .readout {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    height: 26px;
    padding: 0 var(--s-2);
    border: 1px solid transparent;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--t2);
    font-size: 11px;
    white-space: nowrap;
  }

  .button { cursor: pointer; transition: background var(--dur-fast) var(--ease) }
  .button:hover { background: var(--accent-soft); color: var(--text) }

  /* Visually hidden, still read aloud. */
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
