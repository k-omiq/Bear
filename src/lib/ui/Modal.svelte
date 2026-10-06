<script>
  import { captureFocus, cycleTab, focusable } from './focus.js'

  /**
   * Centred dialog. Mount it conditionally - `{#if showing}<Modal …/>{/if}` -
   * rather than toggling an `open` prop; focus is captured on mount and
   * restored to the invoking element on destroy.
   *
   * Dismissal is driven entirely by `onclose`:
   *
   * - no `onclose` - nothing dismisses the dialog. Escape and the backdrop are
   *   both inert. This is the shape a refusal takes (an overwrite refusal must
   *   not be dismissable *into* an overwrite);
   *   the only way out is a button in the `buttons` snippet.
   * - `onclose` + `blocking` - Escape closes because a cancel action exists;
   *   the backdrop is ignored, so no stray click can answer a confirmation.
   * - `onclose` alone - Escape and backdrop both close.
   *
   * Widths, per the metrics table: settings 520, export 460, about 460, new
   * project 440, everything else 400.
   *
   * `fill` lays the dialog over the whole window instead, `width` ignored,
   * and hands its body the height that is left, as a column, so a child can
   * grow into it (the denoise compare view's full window). It is a layout
   * rather than the Fullscreen API, which WKWebView, the engine the app
   * ships on, grants only behind a prefix and a user gesture.
   *
   * @type {{
   *   title: string,
   *   meta?: string,
   *   width?: number,
   *   blocking?: boolean,
   *   fill?: boolean,
   *   onclose?: () => void,
   *   children?: import('svelte').Snippet,
   *   footnote?: import('svelte').Snippet,
   *   buttons?: import('svelte').Snippet,
   * }}
   */
  let {
    title,
    meta,
    width = 400,
    blocking = false,
    fill = false,
    onclose,
    children,
    footnote,
    buttons,
  } = $props()

  const titleId = $props.id()

  /** @type {HTMLElement | undefined} */
  let dialog = $state()

  $effect(() => {
    const restore = captureFocus()
    const previousOverflow = document.body.style.overflow
    document.body.style.overflow = 'hidden'

    // First focusable inside, or the dialog itself when it holds none.
    const first = dialog ? focusable(dialog)[0] : undefined
    ;(first ?? dialog)?.focus()

    return () => {
      document.body.style.overflow = previousOverflow
      restore()
    }
  })

  /** @param {KeyboardEvent} e */
  function onkeydown(e) {
    if (e.key === 'Escape' && onclose) {
      e.preventDefault()
      e.stopPropagation()
      onclose()
      return
    }
    if (dialog) cycleTab(e, dialog)
  }

  /**
   * The identity test comes **first**, before either prop is read.
   *
   * A click on a button inside the dialog bubbles to this handler, and by the
   * time it arrives that button may already have closed the dialog - so
   * `blocking` and `onclose` can be getters over a spec that is no longer on
   * the stack. Reading them first threw, out of an event handler, in the
   * middle of a flush. Nothing about a click that did not start on the
   * backdrop needs either prop.
   *
   * @param {MouseEvent} e
   */
  function onbackdrop(e) {
    if (e.target !== e.currentTarget) return
    if (blocking || !onclose) return
    onclose()
  }

  // Pressing on the backdrop would otherwise blur the dialog and drop focus on
  // <body>, which strands keyboard users outside a dialog they cannot leave.
  /** @param {MouseEvent} e */
  function onbackdropdown(e) {
    if (e.target === e.currentTarget) e.preventDefault()
  }
</script>

<svelte:window onkeydown={onkeydown} />

<!-- The backdrop is a dismissal affordance, not a control: the dialog always
     carries a keyboard route out, so this needs no role of its own. -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="backdrop" onclick={onbackdrop} onmousedown={onbackdropdown}>
  <div
    bind:this={dialog}
    class="dialog"
    class:fill
    role="dialog"
    aria-modal="true"
    aria-labelledby={titleId}
    tabindex="-1"
    style:width={fill ? undefined : `${width}px`}
  >
    <div class="head">
      <h2 class="title" id={titleId}>{title}</h2>
      {#if meta}<div class="meta">{meta}</div>{/if}
    </div>

    {#if children}
      <div class="body" class:no-foot={!footnote && !buttons}>
        <div class="body-inner">{@render children()}</div>
      </div>
    {/if}

    {#if footnote || buttons}
      <div class="foot">
        <div class="footnote">{@render footnote?.()}</div>
        {@render buttons?.()}
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--scrim);
    animation: mcFade var(--dur-fast) var(--ease);
  }

  .dialog {
    display: flex;
    flex-direction: column;
    max-width: 92vw;
    max-height: 92vh;
    overflow: hidden;
    border-radius: var(--r-xl);
    background: var(--surface);
    box-shadow: var(--modal-lift);
    animation: mcIn var(--dur) var(--ease);
  }

  .dialog.fill {
    width: 100vw;
    height: 100vh;
    max-width: none;
    max-height: none;
    border-radius: 0;
  }
  .dialog.fill .body-inner {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  .head {
    display: flex;
    align-items: baseline;
    gap: var(--s-4);
    padding: var(--s-6) var(--s-6) 0;
    flex: none;
  }
  .title {
    flex: 1;
    margin: 0;
    font-size: 13.5px;
    font-weight: 600;
  }
  .meta { font-size: 11px; color: var(--t3) }

  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    scrollbar-gutter: stable;
    margin-top: 10px;
    font-size: 12px;
    color: var(--t2);
    line-height: 1.6;
  }

  .body-inner {
    padding: 0 var(--s-6);
  }

  .no-foot .body-inner {
    padding-bottom: var(--s-6);
  }

  .foot {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-6);
    flex: none;
  }
  .footnote {
    flex: 1;
    font-size: 10.5px;
    color: var(--t3);
    line-height: 1.4;
  }
</style>
