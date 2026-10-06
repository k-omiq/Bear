<script>
  import Icon from '../icons/Icon.svelte'

  /**
   * One transient message. It dismisses itself after `duration`, and the timer
   * pauses while the pointer is over it or focus is inside it - a notice you
   * are reading or operating never vanishes under you.
   *
   * `dismissLabel` has no default on purpose: primitives hold no strings, so
   * the already-translated word arrives from the caller.
   *
   * @type {{
   *   text: string,
   *   dismissLabel: string,
   *   tone?: 'info' | 'warn',
   *   icon?: string,
   *   duration?: number,
   *   actionLabel?: string,
   *   onaction?: () => void,
   *   onclose: () => void,
   * }}
   */
  let { text, dismissLabel, tone = 'info', icon, duration = 6500, actionLabel, onaction, onclose } = $props()

  let paused = $state(false)
  /** Milliseconds the notice has already been on screen unpaused. */
  let elapsed = 0
  /** @type {ReturnType<typeof setTimeout> | undefined} */
  let timer
  let startedAt = 0

  function stop() {
    if (timer === undefined) return
    clearTimeout(timer)
    timer = undefined
    elapsed += Date.now() - startedAt
  }

  function start() {
    const left = duration - elapsed
    if (timer !== undefined || left <= 0) return
    startedAt = Date.now()
    timer = setTimeout(onclose, left)
  }

  $effect(() => {
    if (paused) stop()
    else start()
    return stop
  })
</script>

<!-- The card is a plain container inside the stack's live region. Its pointer
     and focus handlers only pause the dismiss timer, so giving it an ARIA role
     would add a node to the accessibility tree that means nothing. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="notice {tone}"
  onmouseenter={() => (paused = true)}
  onmouseleave={() => (paused = false)}
  onfocusin={() => (paused = true)}
  onfocusout={() => (paused = false)}
>
  {#if icon || tone === 'warn'}
    <span class="mark"><Icon name={icon ?? 'warning-triangle'} size={13} /></span>
  {/if}
  <div class="text">{text}</div>
  {#if actionLabel && onaction}
    <button type="button" class="action" onclick={onaction}>{actionLabel}</button>
  {/if}
  <button type="button" class="close" onclick={onclose} title={dismissLabel} aria-label={dismissLabel}>
    <Icon name="close" size={12} />
  </button>
</div>

<style>
  .notice {
    display: flex;
    gap: var(--s-3);
    align-items: flex-start;
    padding: 9px 10px;
    border-radius: var(--r-md);
    background: var(--surface);
    box-shadow: var(--edge);
    animation: mcIn var(--dur) var(--ease);
  }

  .mark { display: flex; flex: none; margin-top: 1px; color: var(--t3) }
  .warn .mark { color: var(--warn) }

  .text {
    flex: 1;
    min-width: 0;
    font-size: 11px;
    line-height: 1.45;
    color: var(--t2);
  }

  .action {
    flex: none;
    padding: 2px 4px;
    border: none;
    border-radius: var(--r-xs);
    background: transparent;
    color: var(--accent);
    font-size: 11px;
    cursor: pointer;
  }
  .action:hover { text-decoration: underline }
  .action:focus-visible { outline: 2px solid var(--accent) }

  .close {
    display: flex;
    flex: none;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    margin: -2px -2px 0 0;
    padding: 0;
    border: none;
    border-radius: var(--r-xs);
    background: transparent;
    color: var(--t3);
    cursor: pointer;
    transition: color var(--dur-fast) var(--ease);
  }
  .close:hover { color: var(--text) }
</style>
