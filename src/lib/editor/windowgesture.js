/**
 * Moving and sizing a floating thing, by pointer and by keyboard.
 *
 * `FloatingWindow.svelte` owned all of this until the tool bar wanted the
 * moving half of it: a bar that is dragged by a grip, put back by `Escape` and
 * nudged by the arrow keys is the window's gesture exactly, and a second copy
 * of it in a second component is two places for one behaviour to drift in.
 * So the handlers live here and both components attach them.
 *
 * Nothing in this module is reactive. The gesture is one mutable record that
 * exists between a `pointerdown` and its `pointerup`, and no view depends on
 * it - what the view reads is `session.windows[id]`, which these handlers
 * write through `setWindowBox` like any other move.
 *
 * The height is a callback rather than a number because a window with no
 * explicit height is sized by its content, and the first drag on its corner
 * has to start from whatever the browser made of it.
 */

import { onDestroy } from 'svelte'
import {
  session,
  setWindowBox,
  commitWindows,
  raiseWindow,
  foldWindow,
  setWindowOpen,
  resetWindowBox,
} from '../state/session.svelte.js'
import { COARSE_STEP, FINE_STEP } from '../model/windows.js'

/**
 * Pointer capture is what keeps a drag alive when the cursor outruns the
 * window. Both halves throw if the pointer has gone away underneath us, and
 * neither failure is worth losing the gesture - or, on release, the save -
 * over.
 *
 * @param {HTMLElement} element
 * @param {number} pointerId
 * @param {boolean} on
 */
function capture(element, pointerId, on) {
  try {
    if (on) element.setPointerCapture(pointerId)
    else element.releasePointerCapture(pointerId)
  } catch {
    /* the pointer is already gone; the gesture ends either way */
  }
}

/**
 * The handlers for one window id.
 *
 * `foldable` is what separates the two callers. A window's grip folds it with
 * `Enter` or `Space`, and its header comment documents that; the tool bar has
 * no fold at all, so those two keys reach `deltas`, find nothing, and are left
 * to whatever else wants them.
 *
 * `id` is a **getter** rather than a string because the caller's is a prop:
 * reading it once here would capture the value the component mounted with, and
 * the compiler says so (`state_referenced_locally`). It costs one call per
 * gesture and it is honest about where the id lives.
 *
 * `width` is for a window whose box is a measurement rather than a choice -
 * the tool panel (`model/windows.js#CONTENT_SIZED`). Its corner sizes the one
 * thing the user does choose, a width kept apart from the box, and only that:
 * the arrows up and down do nothing on it, and a drag reads only `dx`.
 *
 * @param {{
 *   id: () => string,
 *   measuredHeight: () => number,
 *   foldable?: boolean,
 *   width?: {get: () => number, set: (w: number) => void},
 * }} options
 */
export function windowGesture({ id: windowId, measuredHeight, foldable = false, width }) {
  /** @type {{mode: 'move'|'size', pointerId: number, target: HTMLElement, x: number, y: number, box: {x: number, y: number, w: number, h: number}}|null} */
  let gesture = null

  /**
   * @param {PointerEvent & {currentTarget: HTMLElement}} event
   * @param {'move'|'size'} mode
   */
  function onGestureStart(event, mode) {
    if (event.button !== 0 || gesture !== null) return
    const id = windowId()
    const win = session.windows[id]
    if (!win) return
    event.preventDefault()
    if (mode === 'size') event.stopPropagation()
    raiseWindow(id)
    gesture = {
      mode,
      pointerId: event.pointerId,
      target: event.currentTarget,
      x: event.clientX,
      y: event.clientY,
      box: { x: win.x, y: win.y, w: mode === 'size' && width ? width.get() : win.w, h: win.h ?? measuredHeight() },
    }
    capture(event.currentTarget, event.pointerId, true)
  }

  /** @param {PointerEvent} event */
  function onGestureMove(event) {
    if (!gesture || event.pointerId !== gesture.pointerId) return
    const id = windowId()
    const dx = event.clientX - gesture.x
    const dy = event.clientY - gesture.y
    if (gesture.mode === 'size' && width) {
      width.set(gesture.box.w + dx)
    } else if (gesture.mode === 'size') {
      setWindowBox(id, { w: gesture.box.w + dx, h: gesture.box.h + dy })
    } else {
      setWindowBox(id, { x: gesture.box.x + dx, y: gesture.box.y + dy })
    }
  }

  /** @param {PointerEvent & {currentTarget: HTMLElement}} event */
  function onGestureEnd(event) {
    if (!gesture || event.pointerId !== gesture.pointerId) return
    const active = gesture
    gesture = null
    capture(active.target, active.pointerId, false)
    commitWindows()
  }

  /**
   * Arrow keys on the grip and on the corner, and - where there is a fold -
   * `Enter` or `Space` on the grip to fold. `Escape` is stopped here so the
   * keyboard layer's global cancel does not also clear the selection when all
   * the user meant was "put this window back".
   *
   * @param {KeyboardEvent} event
   * @param {'move'|'size'} mode
   */
  function onGestureKey(event, mode) {
    const id = windowId()
    const win = session.windows[id]
    if (!win) return
    const step = event.shiftKey ? FINE_STEP : COARSE_STEP
    /** @type {Record<string, [number, number]>} */
    const deltas = {
      ArrowLeft: [-step, 0],
      ArrowRight: [step, 0],
      ArrowUp: [0, -step],
      ArrowDown: [0, step],
    }
    if (mode === 'move' && event.key === 'Escape') {
      event.preventDefault()
      event.stopPropagation()
      if (gesture) {
        capture(gesture.target, gesture.pointerId, false)
        gesture = null
      }
      resetWindowBox(id)
      return
    }
    if (foldable && mode === 'move' && (event.key === 'Enter' || event.key === ' ')) {
      event.preventDefault()
      event.stopPropagation()
      foldWindow(id)
      return
    }
    const delta = deltas[event.key]
    if (!delta || (mode === 'size' && width && !delta[0])) return
    event.preventDefault()
    event.stopPropagation()
    if (mode === 'size' && width) {
      width.set(width.get() + delta[0])
    } else if (mode === 'size') {
      setWindowBox(id, { w: win.w + delta[0], h: (win.h ?? measuredHeight()) + delta[1] })
    } else {
      setWindowBox(id, { x: win.x + delta[0], y: win.y + delta[1] })
    }
    commitWindows()
  }

  function destroy() {
    if (!gesture) return
    const active = gesture
    gesture = null
    capture(active.target, active.pointerId, false)
    commitWindows()
  }

  try {
    onDestroy(destroy)
  } catch {
    /* outside component lifecycle (e.g. unit tests without DOM) */
  }

  return { onGestureStart, onGestureMove, onGestureEnd, onGestureKey, destroy }
}

/**
 * Close a window, and put focus where the window came from.
 *
 * Closing destroys whatever inside it had focus, and focus would fall to
 * `<body>`. It goes back to the control that opens this one again: the Pages
 * and Layers toggles in the top-left cluster, and for the tool bar the selected
 * tool in the rail, which is what reopens it. Focus moves first, so the
 * removal is not what moves it.
 *
 * @param {string} id
 */
export function closeWindowFocusing(id) {
  const toggle = globalThis.document?.querySelector(`[data-window-toggle="${id}"]`)
  if (toggle instanceof HTMLElement) toggle.focus({ preventScroll: true })
  setWindowOpen(id, false)
}
