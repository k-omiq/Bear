/**
 * Focus helpers shared by Modal and Menu. Deliberately small: a selector, a
 * query, and Tab-cycling - no third-party trap, no global state.
 */

const FOCUSABLE = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled]):not([type="hidden"])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

/**
 * Every focusable descendant of `root`, in document order, skipping anything
 * hidden.
 * @param {HTMLElement} root
 * @returns {HTMLElement[]}
 */
export function focusable(root) {
  return /** @type {HTMLElement[]} */ (
    Array.from(root.querySelectorAll(FOCUSABLE))
  ).filter((el) => el.offsetParent !== null || el === document.activeElement)
}

/**
 * Keep Tab inside `root`. Call from a keydown handler; returns true when the
 * event was handled.
 * @param {KeyboardEvent} e
 * @param {HTMLElement} root
 * @returns {boolean}
 */
export function cycleTab(e, root) {
  if (e.key !== 'Tab') return false
  const items = focusable(root)
  if (!items.length) {
    e.preventDefault()
    return true
  }
  const first = items[0]
  const last = items[items.length - 1]
  const active = /** @type {HTMLElement | null} */ (document.activeElement)

  // Focus on something inside that is not itself a stop - a heading a screen
  // moved focus to - still wraps: Shift+Tab from it would otherwise leave.
  if (e.shiftKey && (active === first || !root.contains(active) || !items.includes(/** @type {HTMLElement} */ (active)) && first.compareDocumentPosition(/** @type {Node} */ (active)) & Node.DOCUMENT_POSITION_PRECEDING)) {
    e.preventDefault()
    last.focus()
    return true
  }
  // The same the other way: a non-stop after the last stop (an alert under
  // the last control, say) wraps forward instead of leaving.
  if (!e.shiftKey && (active === last || !root.contains(active) || !items.includes(/** @type {HTMLElement} */ (active)) && last.compareDocumentPosition(/** @type {Node} */ (active)) & Node.DOCUMENT_POSITION_FOLLOWING)) {
    e.preventDefault()
    first.focus()
    return true
  }
  return false
}

/**
 * Remember the currently focused element so it can be restored on close.
 * If the element has been removed from the DOM meanwhile, falls back to
 * `fallback` (or its first focusable descendant) if provided, or the
 * previous element's nearest surviving ancestor.
 *
 * @param {HTMLElement | (() => HTMLElement | null | undefined) | null} [fallback]
 * @returns {(override?: HTMLElement | (() => HTMLElement | null | undefined) | null) => void} restore
 */
export function captureFocus(fallback) {
  const previous = /** @type {HTMLElement | null} */ (document.activeElement)
  const parent = previous?.parentElement ?? null
  return (override) => {
    if (previous && document.contains(previous)) {
      previous.focus()
      return
    }
    const target = override !== undefined ? override : fallback
    const node = typeof target === 'function' ? target() : target
    if (node && document.contains(node)) {
      const first = focusable(node)[0]
      ;(first ?? node).focus?.()
      return
    }
    if (parent && document.contains(parent)) {
      const first = focusable(parent)[0]
      if (first) {
        first.focus()
        return
      }
    }
  }
}
