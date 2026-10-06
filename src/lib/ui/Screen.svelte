<script>
  import { captureFocus, cycleTab, focusable } from './focus.js'

  /**
   * A full-window surface over whatever route is underneath.
   *
   * It is `Modal`'s contract at window size: mount it conditionally, focus is
   * captured on mount and restored on destroy, Tab stays inside, and Escape
   * calls `onclose` when there is one. The route underneath stays mounted, so
   * an editor opened behind Settings keeps its state.
   *
   * Layout is the caller's. This owns only the layer.
   *
   * @type {{
   *   label: string,
   *   onclose?: () => void,
   *   children: import('svelte').Snippet,
   * }}
   */
  let { label, onclose, children } = $props()

  /** @type {HTMLElement | undefined} */
  let root = $state()

  $effect(() => {
    const restore = captureFocus()
    const previousOverflow = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    const first = root ? focusable(root)[0] : undefined
    ;(first ?? root)?.focus()
    return () => {
      document.body.style.overflow = previousOverflow
      restore()
    }
  })

  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    // A control inside that answered the key itself (a select closing its
    // list, say) has already used it.
    if (event.defaultPrevented) return
    if (event.key === 'Escape' && onclose) {
      event.preventDefault()
      event.stopPropagation()
      onclose()
      return
    }
    if (root) cycleTab(event, root)
  }
</script>

<svelte:window {onkeydown} />

<div
  bind:this={root}
  class="screen"
  role="dialog"
  aria-modal="true"
  aria-label={label}
  tabindex="-1"
>
  {@render children()}
</div>

<style>
  .screen {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--text);
    animation: mcFade var(--dur) var(--ease);
  }
  .screen:focus { outline: none }
</style>
