/**
 * The open state of an anchored overlay, and the ways one is dismissed.
 *
 * `Menu` and `Popover` differ in what they hold - a list of items with roving
 * focus, a panel of ordinary controls - and agree on most of how they come and
 * go: the element that was focused is remembered on the way up, an outside
 * pointerdown closes without taking focus back (the press has already given it
 * somewhere else), and `Escape` closes and returns it. That agreement was two
 * copies of the same twenty lines until this module.
 *
 * Where they part is `Tab`. A menu is **one control** - Tab leaves it, so Tab
 * dismisses it (`tabAway`). A popover is a *panel of several*, and Tab inside
 * one moves between them; the first Tab out of the first slider used to take
 * the whole Adjustments panel with it. What ends a popover is focus actually
 * leaving the anchor for somewhere else (`focusOut`), which is the event a Tab
 * off the last control produces; a pointer press elsewhere is the outside
 * listener's, as it is for a menu.
 *
 * The outside listener is registered in a `$effect`, so this must be called
 * during a component's initialisation - which is the only place either
 * component calls it.
 */

import { captureFocus } from './focus.js'

/**
 * @param {() => HTMLElement|undefined} root - the anchor, trigger included:
 *   a pointerdown inside it is not an outside press
 * @param {() => HTMLElement|undefined} [portal] - an optional panel rendered outside the anchor
 */
export function anchoredOverlay(root, portal = () => undefined) {
  let open = $state(false)
  /** @type {(() => void) | null} */
  let restore = null
  let openedAtPointerDown = false

  function show() {
    if (open) return
    restore = captureFocus(() => root())
    open = true
  }

  /** @param {{refocus?: boolean}} [options] */
  function close({ refocus = true } = {}) {
    if (!open) return
    open = false
    if (refocus) restore?.()
    restore = null
  }

  function toggle() {
    if (openedAtPointerDown) {
      close()
    } else {
      open ? close() : show()
    }
    openedAtPointerDown = false
  }

  /**
   * `Escape`, which means the same thing to both components: close, and put
   * focus back where it was.
   *
   * @param {KeyboardEvent} event
   * @returns {boolean} whether the key was a dismissal - the caller should
   *   then do nothing else with it
   */
  function dismissKey(event) {
    if (event.key !== 'Escape' || !open) return false
    event.preventDefault()
    event.stopPropagation()
    event.stopImmediatePropagation?.()
    close()
    return true
  }

  /**
   * `Tab` as a dismissal, which is a **menu's** reading of it and not a
   * popover's. Focus must land where Tab sent it, so nothing is given back.
   *
   * The `stopPropagation` keeps the key from reaching an outer keydown
   * listener that would read it a second time. A popover holding this menu is
   * not one of those - it ends on `focusOut` rather than on `Tab` - but a menu
   * nested in anything that does listen would otherwise be dismissed twice by
   * the one press.
   *
   * @param {KeyboardEvent} event
   * @returns {boolean} whether the key was a dismissal
   */
  function tabAway(event) {
    if (event.key !== 'Tab' || !open) return false
    event.stopPropagation()
    close({ refocus: false })
    return true
  }

  /**
   * Focus leaving the anchor altogether, which is how an overlay that holds
   * several controls ends: Tab moves between them for as long as the next one
   * is inside, and the first move to anything else closes the panel behind it.
   *
   * A null `relatedTarget` or `document.body` is **not** a dismissal. It is what
   * a press on the page background produces, and it is also what WebKit - which
   * is the engine under the shipped application, since Tauri draws in a
   * `WKWebView` on macOS - produces for a press on the panel's *own* slider
   * thumb, label, padding or trigger button, because WebKit does not focus a
   * button or a range input on click. Reading null or body as outside closed
   * the panel under the very pointer that was using it there, and the trigger's
   * click then re-opened what its press had just closed. The outside press has
   * an owner already, the capture-phase `pointerdown` listener below, so this
   * handler answers only the question it can answer: has focus arrived somewhere
   * that is not in the anchor? Nothing is given back: focus has already gone
   * where the user sent it.
   *
   * @param {FocusEvent} event
   * @returns {boolean} whether it was a dismissal
   */
  function focusOut(event) {
    if (!open) return false
    const next = /** @type {Node|null} */ (event.relatedTarget)
    if (!next || next === document.body || next === document.documentElement || root()?.contains(next) || portal()?.contains(next)) {
      return false
    }
    close({ refocus: false })
    return true
  }

  $effect(() => {
    if (!open) return
    /** @param {PointerEvent} event */
    const outside = (event) => {
      const node = root()
      const detached = portal()
      if (node && !node.contains(/** @type {Node} */ (event.target)) && !detached?.contains(/** @type {Node} */ (event.target))) {
        close({ refocus: false })
      } else if (node?.contains(/** @type {Node} */ (event.target))) {
        openedAtPointerDown = open
      }
    }
    document.addEventListener('pointerdown', outside, true)
    return () => {
      document.removeEventListener('pointerdown', outside, true)
      openedAtPointerDown = false
      restore = null
    }
  })

  return {
    get open() {
      return open
    },
    show,
    close,
    toggle,
    dismissKey,
    tabAway,
    focusOut,
  }
}
