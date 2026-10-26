/**
 * How a mouse moves the canvas: the wheel, the middle button, a drag on empty
 * canvas, and the held pan key (`shortcuts.js#HOLD_PAN_ID`).
 *
 * A trackpad already scrolls with two fingers and zooms with a pinch, and the
 * canvas has always taken both. A mouse has one wheel axis and nothing to
 * grab the page with, so before this a mouse user could not move a zoomed
 * page sideways at all. Each of the three choices below is the user's, kept
 * in `session.navigation`; the two presets are the common combinations, named
 * by the device they suit.
 *
 * Pure, like `shortcuts.js`: what a wheel or a press *means* is decided here
 * from the event's fields, so it is tested without a canvas. `CanvasStage`
 * does the scrolling.
 *
 * @typedef {Object} Navigation
 * @property {'scroll'|'zoom'} wheel - what a plain wheel turn does. ⌘ or Ctrl with it (and a pinch) always zooms
 * @property {boolean} middlePan - a middle button drag pans
 * @property {boolean} emptyPan - a left drag that starts on nothing clickable pans
 */

/** @type {ReadonlyArray<'scroll'|'zoom'>} */
export const WHEEL_MODES = ['scroll', 'zoom']

/**
 * The presets, in the order the sheet offers them. `trackpad` is the default
 * and is what the canvas did before the choice existed, plus the middle
 * button, which a trackpad does not have and so cannot press by accident.
 *
 * @type {Readonly<Record<'trackpad'|'mouse', Navigation>>}
 */
export const NAVIGATION_PRESETS = {
  trackpad: { wheel: 'scroll', middlePan: true, emptyPan: false },
  mouse: { wheel: 'zoom', middlePan: true, emptyPan: true },
}

/** @returns {Navigation} */
export function defaultNavigation() {
  return { ...NAVIGATION_PRESETS.trackpad }
}

/**
 * Coerce anything - a hand-edited settings file, a record from before this
 * setting - into a `Navigation`. Each field falls back on its own.
 *
 * @param {unknown} raw
 * @returns {Navigation}
 */
export function normalizeNavigation(raw) {
  const base = defaultNavigation()
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return base
  const record = /** @type {Record<string, unknown>} */ (raw)
  return {
    wheel: WHEEL_MODES.includes(/** @type {any} */ (record.wheel)) ? /** @type {any} */ (record.wheel) : base.wheel,
    middlePan: typeof record.middlePan === 'boolean' ? record.middlePan : base.middlePan,
    emptyPan: typeof record.emptyPan === 'boolean' ? record.emptyPan : base.emptyPan,
  }
}

/**
 * The preset these settings are, or `null` when the user has mixed their own.
 *
 * @param {Navigation} navigation
 * @returns {'trackpad'|'mouse'|null}
 */
export function navigationPreset(navigation) {
  for (const [id, preset] of Object.entries(NAVIGATION_PRESETS)) {
    if (
      preset.wheel === navigation.wheel
      && preset.middlePan === navigation.middlePan
      && preset.emptyPan === navigation.emptyPan
    ) return /** @type {'trackpad'|'mouse'} */ (id)
  }
  return null
}

/**
 * Does this wheel event zoom?
 *
 * ⌘ or Ctrl always zooms: a trackpad pinch arrives as a Ctrl wheel, and the
 * two cannot be told apart. In `zoom` mode a plain turn zooms too, and Shift
 * or Alt hands the turn back to the scroller - Shift to scroll sideways, Alt
 * to scroll down - so a mouse can still scroll without grabbing the page.
 * A purely sideways event (a tilt wheel, a trackpad swipe) never zooms.
 *
 * @param {{deltaX?: number, deltaY?: number, ctrlKey?: boolean, metaKey?: boolean, shiftKey?: boolean, altKey?: boolean}} event
 * @param {Navigation} navigation
 * @returns {boolean}
 */
export function wheelZooms(event, navigation) {
  if (event.ctrlKey || event.metaKey) return true
  if (navigation.wheel !== 'zoom' || event.shiftKey || event.altKey) return false
  const dy = Math.abs(event.deltaY ?? 0)
  return dy > 0 && dy >= Math.abs(event.deltaX ?? 0)
}

/**
 * The controls a press may land on and still be the control's: a region, the
 * drawing surface, a handle, a menu. A drag that starts on one of these is
 * that control's drag, never a pan. The page's own empty space is a button
 * too (`RegionLayer`'s clear-selection target), marked `data-canvas-empty`:
 * a click there still clears the selection, and a drag pans.
 */
const CONTROLS = 'button, a[href], input, select, textarea, [contenteditable], [role="slider"], [role="menu"], [role="menuitem"], [role="dialog"]'

/**
 * Does this press start a pan, and how?
 *
 * - `grab`: take the press outright, before the control under it sees it -
 *   the middle button, or the left one with the pan key held.
 * - `empty`: a left press on nothing clickable, with `emptyPan` on. The
 *   control under it, if any, has already refused it, so nothing is taken.
 * - `null`: not a pan.
 *
 * @param {{button: number, target?: any}} event
 * @param {{navigation: Navigation, held: boolean}} context
 * @returns {'grab'|'empty'|null}
 */
export function panStart(event, { navigation, held }) {
  if (event.button === 1) return navigation.middlePan ? 'grab' : null
  if (event.button !== 0) return null
  if (held) return 'grab'
  if (!navigation.emptyPan) return null
  const target = event.target
  if (typeof target?.closest !== 'function') return 'empty'
  return target.closest('[data-canvas-empty]') || !target.closest(CONTROLS) ? 'empty' : null
}
