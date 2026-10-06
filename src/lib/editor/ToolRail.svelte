<script>
  /**
   * The tool rail: five tools down the right edge, `1`–`5`.
   *
   * It is a radio group - exactly one tool is chosen, and the arrow keys move
   * between them rather than Tab, so the rail is a single stop in the focus
   * order. Choosing a tool also opens and raises the tool window; that happens
   * in `setTool`, so the keyboard route does it too. That makes the selected
   * tool the control that reopens the tool window, which has no toggle in the
   * clusters - hence `data-window-toggle` on it, which is where focus goes
   * when the tool window is closed (see `FloatingWindow.svelte`).
   */
  import { editor, setTool } from '../state/editor.svelte.js'
  import { TOOL_SPECS, TOOL_ICONS } from './tools.js'
  import { IconButton } from '../ui/index.js'
  import { t } from '../i18n/index.js'

  /** @type {HTMLElement|undefined} */
  let rail = $state()

  const selected = $derived(TOOL_SPECS.findIndex((spec) => spec.id === editor.tool))

  /** @param {number} index */
  function focusAt(index) {
    const count = TOOL_SPECS.length
    const next = ((index % count) + count) % count
    setTool(TOOL_SPECS[next].id)
    /** @type {HTMLElement[]} */
    const buttons = rail ? [...rail.querySelectorAll('button')] : []
    buttons[next]?.focus()
  }

  /**
   * @param {KeyboardEvent} event
   * @param {number} from
   */
  function onkeydown(event, from) {
    switch (event.key) {
      case 'ArrowDown':
      case 'ArrowRight':
        claim(event); focusAt(from + 1); break
      case 'ArrowUp':
      case 'ArrowLeft':
        claim(event); focusAt(from - 1); break
      case 'Home':
        claim(event); focusAt(0); break
      case 'End':
        claim(event); focusAt(TOOL_SPECS.length - 1); break
      default:
    }
  }

  /**
   * The rail has answered this key. Stopping it here is what keeps the
   * shortcut layer on `window` from paging the chapter with the same arrow
   * that moved between tools.
   *
   * @param {KeyboardEvent} event
   */
  function claim(event) {
    event.preventDefault()
    event.stopPropagation()
  }
</script>

<div class="rail" aria-label={t('editor.region.tools')}>
  <div bind:this={rail} class="tools" role="radiogroup" aria-label={t('editor.region.tools')}>
    {#each TOOL_SPECS as spec, index (spec.id)}
      <IconButton
        icon={TOOL_ICONS[spec.id]}
        label={t(spec.nameKey)}
        shortcut={String(spec.slot)}
        size={34}
        active={spec.id === editor.tool}
        role="radio"
        aria-checked={spec.id === editor.tool}
        tabindex={index === (selected < 0 ? 0 : selected) ? 0 : -1}
        data-window-toggle={spec.id === editor.tool ? 'tool' : undefined}
        onclick={() => setTool(spec.id)}
        onkeydown={(e) => onkeydown(e, index)}
      />
    {/each}
  </div>

</div>

<style>
  .rail {
    position: absolute;
    right: 16px;
    top: 50%;
    transform: translateY(-50%);
    z-index: 40;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s-1);
    padding: var(--s-2);
    border-radius: 19px;
    background: var(--panel);
    box-shadow: var(--edge);
  }

  .tools {
    display: flex;
    flex-direction: column;
    gap: var(--s-1);
  }
</style>
