<script>
  import Notice from './Notice.svelte'

  /**
   * The transient stack: bottom-left, 230 wide, newest at the bottom. The
   * container is itself the polite live region, so a notice is announced once,
   * as it arrives, without duplicating its text elsewhere in the a11y tree.
   *
   * Offsets are props so the shell can lift the stack clear of the editor's
   * bottom bar; the default 14/14 is the metrics-table anchor.
   *
   * @type {{
   *   notices: Array<{ id: string, text: string, tone?: 'info' | 'warn', icon?: string, duration?: number, actionLabel?: string, onaction?: () => void }>,
   *   onclose: (id: string) => void,
   *   dismissLabel: string,
   *   left?: number,
   *   bottom?: number,
   * }}
   */
  let { notices, onclose, dismissLabel, left = 14, bottom = 14 } = $props()
</script>

<div
  class="stack"
  style:left="{left}px"
  style:bottom="{bottom}px"
  aria-live="polite"
  aria-atomic="false"
>
  {#each notices as n (n.id)}
    <Notice
      text={n.text}
      tone={n.tone}
      icon={n.icon}
      duration={n.duration}
      actionLabel={n.actionLabel}
      onaction={n.onaction}
      {dismissLabel}
      onclose={() => onclose(n.id)}
    />
  {/each}
</div>

<style>
  .stack {
    position: fixed;
    z-index: 10;
    width: 230px;
    max-width: calc(100vw - 28px);
    display: flex;
    flex-direction: column;
    gap: 7px;
    pointer-events: none;
  }
  /* The container spans a fixed column even when empty; only the cards
     themselves should intercept the pointer. */
  .stack > :global(*) { pointer-events: auto }
</style>
