<script>
  import Icon from '../icons/Icon.svelte'
  import RangeField from './RangeField.svelte'
  import { captureFocus } from './focus.js'

  /**
   * A menu that opens **where the pointer is**, rather than under a trigger.
   *
   * `Menu.svelte` is the anchored dropdown: it owns its open state and hangs
   * off a trigger button it renders itself. A context menu has neither - it is
   * raised by a `contextmenu` event on something else entirely, at a position
   * only that event knows - so it is a controlled component: the caller decides
   * when it exists (`{#if}`) and where (`x`, `y`), and this owns the keyboard,
   * the dismissal and the clamping to the viewport.
   *
   * Items arrive in **sections**. A section with a `label` is a labelled group
   * whose items are `menuitemradio` - the engine choices - and a section
   * without one is a plain run of `menuitem`s; a hairline separates them. Every
   * item keeps the mark column, so the labels of a mixed menu start at one x.
   *
   * An item with a `range` is a `RangeField` rather than an entry: it takes
   * its place in the arrows' focus order like any item, Left and Right and the
   * wheel step it, its box takes a typed value, and the value it settles on -
   * the thumb let go, Enter, a run of steps once it stops - goes to `onrange`.
   * The menu stays open, so the change can be seen and changed again.
   *
   * No state, no strings: every label arrives translated, like the rest of the
   * vocabulary.
   *
   * @type {{
   *   x: number,
   *   y: number,
   *   label: string,
   *   sections: Array<{
   *     id: string,
   *     label?: string | null,
   *     items: Array<{
   *       id: string, label: string, icon?: string, disabled?: boolean, selected?: boolean,
   *       range?: { min: number, max: number, step?: number, value: number, unit?: string, hint?: string, boxLabel?: string },
   *     }>,
   *   }>,
   *   onselect: (id: string) => void,
   *   onrange?: (id: string, value: number) => void,
   *   onclose: () => void,
   * }}
   */
  let { x, y, label, sections, onselect, onrange, onclose } = $props()

  /** How close to the window edge the panel may come. */
  const MARGIN = 6

  /** @type {HTMLElement | undefined} */
  let root = $state()
  /** @type {Array<{focus: (options?: FocusOptions) => void}>} */
  let itemEls = $state([])
  let at = $state(0)
  /**
   * Where the panel ended up once it had a size to clamp against - null until
   * it has been measured, when the asked-for point is the best answer there
   * is. Held apart from `x`/`y` so the first paint is at the pointer rather
   * than at the origin.
   *
   * @type {{left: number, top: number}|null}
   */
  let measured = $state(null)
  const placed = $derived(measured ?? { left: x, top: y })

  // One flat list of the focusable items, in the order they are rendered, so
  // the arrows cross section boundaries the way a menu's arrows should.
  const flat = $derived(
    sections.flatMap((section) => section.items.map((item) => ({ ...item, section: section.id }))),
  )
  const enabled = $derived(
    flat.map((item, index) => ({ item, index })).filter((entry) => !entry.item.disabled),
  )

  const restore = captureFocus()

  // The first item takes focus as the menu appears: a menu opened from the
  // keyboard is unusable otherwise, and one opened by the pointer needs the
  // focus anyway for Escape to return it where it came from. Without
  // scrolling: the panel is fixed and on screen, but a region's menu is
  // mounted inside the canvas scroller, and WebKit scrolled that to it.
  $effect(() => {
    itemEls[at]?.focus({ preventScroll: true })
  })

  // Clamped after it is measured, not before: the panel's height depends on
  // how many engines this machine offers, and a menu opened near the bottom of
  // the window would otherwise hang off it.
  $effect(() => {
    if (!root) return
    const box = root.getBoundingClientRect()
    const width = globalThis.innerWidth ?? box.width
    const height = globalThis.innerHeight ?? box.height
    measured = {
      left: Math.max(MARGIN, Math.min(x, width - box.width - MARGIN)),
      top: Math.max(MARGIN, Math.min(y, height - box.height - MARGIN)),
    }
  })

  // Everything that means "the thing this menu is about has moved out from
  // under it". A context menu is anchored to a point in the window, so a
  // scroll or a resize leaves it pointing at nothing.
  $effect(() => {
    // A press outside is going somewhere; pulling the focus back to where the
    // menu was opened from would fight the click that dismissed it.
    /** @param {Event} event */
    const outside = (event) => {
      if (root && !root.contains(/** @type {Node} */ (event.target))) dismiss({ refocus: false })
    }
    const away = () => dismiss()
    // A typed value longer than its box scrolls the box itself: that is not
    // the page moving out from under the menu.
    /** @param {Event} event */
    const scrolled = (event) => {
      if (root && event.target instanceof Node && root.contains(event.target)) return
      dismiss()
    }
    document.addEventListener('pointerdown', outside, true)
    globalThis.addEventListener('scroll', scrolled, true)
    globalThis.addEventListener('resize', away)
    globalThis.addEventListener('blur', away)
    return () => {
      document.removeEventListener('pointerdown', outside, true)
      globalThis.removeEventListener('scroll', scrolled, true)
      globalThis.removeEventListener('resize', away)
      globalThis.removeEventListener('blur', away)
    }
  })

  /** Close without running anything, and put the focus back where it was. */
  function dismiss({ refocus = true } = {}) {
    if (refocus) restore()
    onclose()
  }

  /** @param {number} delta */
  function step(delta) {
    if (enabled.length === 0) return
    const here = enabled.findIndex((entry) => entry.index === at)
    const next = here === -1 ? 0 : (here + delta + enabled.length) % enabled.length
    at = enabled[next].index
  }

  /** @param {KeyboardEvent} event */
  function onkeydown(event) {
    // A slider or its box answered the key itself.
    if (event.defaultPrevented && event.key !== 'Escape') return
    // Home and End move the caret in a typed value, not the menu's focus.
    const typing = /** @type {any} */ (event.target)?.type === 'text'
    if (typing && (event.key === 'Home' || event.key === 'End')) return
    switch (event.key) {
      case 'Escape':
        dismiss()
        break
      case 'ArrowDown':
        step(1)
        break
      case 'ArrowUp':
        step(-1)
        break
      case 'Home':
        at = enabled[0]?.index ?? 0
        break
      case 'End':
        at = enabled[enabled.length - 1]?.index ?? 0
        break
      case 'Tab': {
        // Inside a range item, Tab goes between its slider and the box beside
        // it, which has no other way in from the keyboard.
        const field = /** @type {Element|null} */ (event.target instanceof Element ? event.target.closest('.range') : null)
        const typing = /** @type {any} */ (event.target)?.type === 'text'
        if (field && typing === event.shiftKey) return
        // Tabbing away is a dismissal, but focus must land where Tab sent it.
        dismiss({ refocus: false })
        return
      }
      default:
        return
    }
    event.preventDefault()
    // The editor's global keys are on the window. An arrow this menu has used
    // must not also page the chapter underneath it.
    event.stopPropagation()
  }

  /**
   * Run the entry, **then** close. The host owns what the menu is about, and
   * closing first would take it away before the entry could read it.
   *
   * @param {string} id
   */
  function pick(id) {
    restore()
    // A range row still settling a step or a typed value hands it over first,
    // so the entry acts on the value the menu shows.
    for (const item of itemEls) if (item && typeof (/** @type {any} */ (item)).flush === 'function') (/** @type {any} */ (item)).flush()
    onselect(id)
    onclose()
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  bind:this={root}
  class="menu"
  role="menu"
  aria-label={label}
  tabindex="-1"
  style:left="{placed.left}px"
  style:top="{placed.top}px"
  {onkeydown}
  oncontextmenu={(event) => event.preventDefault()}
>
  {#each sections as section, si (section.id)}
    {#if si > 0}<div class="sep" role="separator"></div>{/if}
    <div role={section.label ? 'group' : 'none'} aria-label={section.label || undefined}>
      {#if section.label}
        <div class="heading" aria-hidden="true">{section.label}</div>
      {/if}
      {#each section.items as item (item.id)}
        {@const index = flat.findIndex((entry) => entry.id === item.id)}
        {#if item.range}
          <div class="range">
            <RangeField
              bind:this={itemEls[index]}
              label={item.label}
              boxLabel={item.range.boxLabel}
              value={item.range.value}
              min={item.range.min}
              max={item.range.max}
              step={item.range.step ?? 1}
              unit={item.range.unit ?? ''}
              hint={item.range.hint}
              layout="stacked"
              listItem
              wheel="focused"
              disabled={item.disabled}
              onchange={(value) => onrange?.(item.id, value)}
            />
          </div>
        {:else}
        <button
          bind:this={itemEls[index]}
          type="button"
          role={item.selected === undefined ? 'menuitem' : 'menuitemradio'}
          aria-checked={item.selected === undefined ? undefined : item.selected}
          class="item"
          tabindex="-1"
          disabled={item.disabled}
          onclick={() => pick(item.id)}
        >
          <span class="mark">
            {#if item.selected}<Icon name="check" size={13} />{/if}
            {#if item.icon && !item.selected}<Icon name={item.icon} size={13} />{/if}
          </span>
          <span class="item-label">{item.label}</span>
        </button>
        {/if}
      {/each}
    </div>
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 55;
    min-width: 168px;
    max-width: 264px;
    padding: var(--s-1);
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--edge);
    animation: mcIn var(--dur) var(--ease);
    /* The canvas hands the pointer to the drawing surface by switching the
       region layer off (`RegionLayer`'s `.inert`). The menu is rendered inside
       it, and a menu nobody can click is not a menu. */
    pointer-events: auto;
  }

  .heading {
    padding: 5px var(--s-3) 3px;
    color: var(--t3);
    font-size: 10px;
    letter-spacing: .04em;
    text-transform: uppercase;
  }

  .item {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    width: 100%;
    height: 26px;
    padding: 0 var(--s-3);
    border: none;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--t2);
    font-size: 12px;
    text-align: start;
    white-space: nowrap;
    cursor: pointer;
    transition:
      background var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .item:hover:not(:disabled),
  .item:focus-visible { background: var(--accent-soft); color: var(--text) }
  .item:disabled { opacity: .4; cursor: default }

  .item-label { flex: 1; overflow: hidden; text-overflow: ellipsis }

  /* One column for both the check and an item's own icon: an engine list and
     three plain actions share a menu, and their labels have to start at one x. */
  .mark {
    display: flex;
    flex: none;
    align-items: center;
    justify-content: center;
    width: 14px;
    color: var(--text);
  }

  .range {
    padding: 4px var(--s-3) 6px calc(var(--s-3) + 14px + var(--s-3));
  }

  .sep {
    height: 1px;
    margin: var(--s-1) var(--s-2);
    background: var(--line);
  }
</style>
