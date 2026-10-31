<script>
  /**
   * The two saved colours, overlapped as an image editor draws them: the active
   * one in front, the other behind.
   *
   * The front swatch opens the picker for the active colour, and the back one
   * swaps them in one press, so either colour is edited by making it active.
   * The Brush and the Shape paint with the active colour
   * (`editor.svelte.js#setActiveColor`). The picker floats over the page
   * (`unclipped`): the rail is at the window's edge.
   */
  import { setActiveColor, swapColors } from '../state/editor.svelte.js'
  import { session } from '../state/session.svelte.js'
  import { shortcutHint } from '../shortcuts.js'
  import { ColorPicker } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  const swapHint = $derived(session.shortcuts && shortcutHint(['tool.swapColors']))
  const swapLabel = $derived(t('tools.color.pairSwap'))
</script>

<div class="pair" role="group" aria-label={t('tools.color.pairGroup')} data-color-switch>
  <button
    type="button"
    class="other"
    style:background-color={session.colorPair.other}
    title={swapHint ? `${swapLabel} · ${swapHint}` : swapLabel}
    aria-label={swapLabel}
    aria-keyshortcuts={swapHint || undefined}
    onclick={swapColors}
  ></button>
  <div class="active">
    <ColorPicker
      value={session.colorPair.active}
      label={t('tools.color.pairActive')}
      align="end"
      unclipped
      onchange={setActiveColor}
    />
  </div>
</div>

<style>
  .pair {
    position: relative;
    width: 34px;
    height: 40px;
    margin-top: var(--s-1);
  }

  .active {
    position: absolute;
    left: 0;
    top: 0;
    display: flex;
    border-radius: var(--r-chip);
    box-shadow: 0 0 0 2px var(--panel);
  }

  .other {
    -webkit-appearance: none;
    appearance: none;
    position: absolute;
    right: 0;
    bottom: 0;
    width: 24px;
    height: 24px;
    padding: 0;
    border: 1px solid var(--line2);
    border-radius: var(--r-chip);
    box-sizing: border-box;
    cursor: pointer;
    transition: border-color var(--dur-fast) var(--ease);
  }
  .other:hover {
    border-color: var(--tintline);
  }
  .other:focus-visible {
    z-index: 1;
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
</style>
