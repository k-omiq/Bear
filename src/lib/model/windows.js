/**
 * Geometry for the editor's floating windows: where they start, and how far
 * they may be dragged and resized.
 *
 * All of it is arithmetic over plain numbers - no Svelte, no DOM - so the
 * clamps can be tested without a browser and so the drag handler in
 * `src/lib/editor/FloatingWindow.svelte` contains no rules of its own.
 *
 * Every number comes from `design/Manga Cleaner Studio.dc.html` (`drag()` and
 * the initial `win` state) by way of `.superpowers/sdd/frontend/editor-chrome-spec.md`.
 */

/** The floating windows, in their initial stacking order (lowest first). */
export const WINDOW_IDS = /** @type {const} */ (['pages', 'layers', 'tool'])

/** Resize bounds. `h` is additionally capped against the viewport. */
export const MAX_WIDTH = 560
export const MIN_HEIGHT = 120

/**
 * The floor a window with no rule of its own gets: wide enough for a list of
 * page or layer rows, which is what the two left-hand windows hold.
 */
export const DEFAULT_MIN_WIDTH = 198

/**

 * The windows whose size is decided by what is **inside** them.
 *
 * The tool shell is the one. For most tools it is a one-row pill, `width:
 * max-content`, exactly as long as the tool selected needs it to be; for Text
 * cleanup it is a compact panel of rows, a fixed width and as tall as its rows.
 * It changes both dimensions as the tool changes. Nothing may squeeze that - a
 * floor would cut the last control off the end of a long tool, and the 560
 * ceiling would do the same - so `clampSize` passes both through for these ids
 * and the shell writes back what the browser actually measured
 * (`editor/ToolBar.svelte`), which is what `clampPosition` needs in order to
 * keep the whole of it on screen.
 *
 * It used to have the **widest** floor of the three instead, computed from the
 * label / control / readout grid every row in it was drawn on. There are no
 * rows and no grid now, and a bar has no resize corner to defend against.
 */
export const CONTENT_SIZED = /** @type {const} */ (['tool'])

/**
 * @param {string} [id]
 * @returns {boolean} whether this window's width is its content's business
 */
export function contentSized(id) {
  return id !== undefined && /** @type {readonly string[]} */ (CONTENT_SIZED).includes(id)
}

/**
 * The floor for a window id, and none at all for a content-sized one.
 *
 * @param {string} [id]
 * @returns {number}
 */
export function minWidthFor(id) {
  return contentSized(id) ? 0 : DEFAULT_MIN_WIDTH
}

/**
 * How much of a window must stay on screen. A window may hang off either edge
 * - that is what makes a 560px panel usable on a small display - but never so
 * far that there is nothing left to grab.
 */
export const KEEP_ON_SCREEN = 84

/** Vertical drag bounds: the header may not go above 4px or below vh − 38. */
export const MIN_TOP = 4
export const BOTTOM_INSET = 38

/** Room for the two bottom control pills and a small gap below Text cleanup. */
export const TOOL_BOTTOM_RESERVE = 62

/** A window's height may not exceed the viewport less this. */
export const HEIGHT_INSET = 30

/** Keyboard move / resize increments (the fine one is Shift). */
export const COARSE_STEP = 8
export const FINE_STEP = 1

/**
 * @typedef {Object} WindowGeometry
 * @property {number} x
 * @property {number} y
 * @property {number} w
 * @property {number|null} h - null means "size to content, capped at 64vh"
 */

/**
 * The layout a fresh install starts from, computed from the viewport once.
 *
 * Pages sits under the top-left cluster, Layers directly beneath it, and the
 * tool bar centred across the top - it is
 * a bar rather than a panel, it belongs to the page rather than to a corner,
 * and the top edge is where the eye already is between the two clusters. Its
 * `w` is a first guess for the clamp and nothing else: the bar measures itself
 * on mount and writes the real width back.
 *
 * @param {number} vw
 * @param {number} vh
 * @returns {Record<string, WindowGeometry>}
 */
export function defaultGeometry(vw, vh) {
  const pagesHeight = Math.max(200, Math.round(vh * 0.32))
  return {
    pages: { x: 16, y: 62, w: 248, h: pagesHeight },
    layers: { x: 16, y: 74 + pagesHeight, w: 248, h: Math.max(180, Math.round(vh * 0.34)) },
    tool: { x: Math.max(16, Math.round(vw / 2 - 280)), y: 62, w: 560, h: null },
  }
}

/**
 * @param {number} value
 * @param {number} min
 * @param {number} max
 * @returns {number}
 */
function clamp(value, min, max) {
  // `min` wins when the range inverts - a viewport narrower than the window.
  return Math.max(min, Math.min(max, value))
}

/**
 * Clamp a window's top-left corner into a viewport.
 *
 * A **resizable** window may hang off either edge - that is what makes a
 * 560px panel usable on a small display - but never so far that less than
 * `KEEP_ON_SCREEN` of it is left to grab.
 *
 * A **content-sized** window (`contentSized`) is held entirely on screen
 * instead, whenever it fits, in both dimensions. Its size is not the user's
 * choice: the tool shell grows when the selected tool brings more controls,
 * again when a run puts its progress beside the button, and from a pill into
 * a panel when Text cleanup is chosen. A shell placed near the right or the
 * bottom edge a moment ago would otherwise grow straight off it with its run
 * button out of reach. When it is wider than the viewport it is free to travel
 * between `vw - width` and `0`, so either end can be brought into view and
 * neither is pinned; when it is taller, its top (where the grip is) stays at
 * `MIN_TOP`. A height that has not been measured yet (`h` absent or null)
 * holds only the general top and bottom bounds. The `id` is what selects the
 * rule; without one the general rule applies.
 *
 * @param {{x: number, y: number, w: number, h?: number|null}} box
 * @param {number} vw
 * @param {number} vh
 * @param {string} [id] - the window being placed; omitted means a resizable one
 * @returns {{x: number, y: number}}
 */
export function clampPosition({ x, y, w, h = null }, vw, vh, id) {
  const top = clamp(Math.round(y), MIN_TOP, vh - BOTTOM_INSET)
  if (contentSized(id)) {
    const width = Math.max(0, Math.round(w))
    const height = h === null || !(h > 0) ? 0 : Math.round(h)
    // The tool pill is 44px high; a measured shell taller than 100px is its
    // Text cleanup panel and must clear the bottom view controls.
    return {
      x: clamp(Math.round(x), Math.min(0, vw - width), Math.max(0, vw - width)),
      y: height > 0 ? clamp(Math.round(y), MIN_TOP, Math.max(MIN_TOP, vh - height - (id === 'tool' && height > 100 ? TOOL_BOTTOM_RESERVE : MIN_TOP))) : top,
    }
  }
  // A window narrower than the strip that must stay reachable is floored at
  // that strip, because `-(w - KEEP_ON_SCREEN)` inverts below it. Written as a
  // subtraction rather than as `-(width - KEEP_ON_SCREEN)` so that a window
  // exactly the width of the strip lands on 0 and not on -0.
  const left = KEEP_ON_SCREEN - Math.max(w, KEEP_ON_SCREEN)
  return {
    x: clamp(Math.round(x), left, vw - KEEP_ON_SCREEN),
    y: top,
  }
}

/**
 * Clamp a window's size. A null height stays null - the window is sizing
 * itself to its content and has never been resized.
 *
 * The id is what decides whether the width is clamped at all: a content-sized
 * window's width is a measurement rather than a preference, and squeezing it
 * would only put the store at odds with the element. The caller is the only one
 * that knows which window it is holding.
 *
 * A content-sized window's **height** is a measurement for the same reason:
 * 44 for a pill, whatever its rows add up to for the Text cleanup panel. It is
 * kept so `clampPosition` can hold the whole shell above the bottom edge, and
 * passed through unclamped; anything that is not a positive number is null,
 * "not measured yet". A geometry stored before the shell existed carries the
 * old tool window's height, and `sanitizeSession` drops it on read, so the
 * only number that reaches here is one the element measured.
 *
 * @param {{w: number, h: number|null}} box
 * @param {number} vh
 * @param {string} [id] - the window being sized; omitted means the general floor
 * @returns {{w: number, h: number|null}}
 */
export function clampSize({ w, h }, vh, id) {
  if (contentSized(id)) {
    return { w: Math.max(0, Math.round(w)), h: h === null || !(h > 0) ? null : Math.round(h) }
  }
  return {
    w: clamp(Math.round(w), DEFAULT_MIN_WIDTH, MAX_WIDTH),
    h: h === null ? null : clamp(Math.round(h), MIN_HEIGHT, vh - HEIGHT_INSET),
  }
}

/**
 * Re-rank the stacking order so that `id` is on top.
 *
 * Ranks are dense and start at 1, so `z-index: 20 + rank` can never climb into
 * the fixed clusters at 40 however many times a window is raised - which a
 * monotonically increasing counter would eventually do.
 *
 * @param {Record<string, number>} ranks - id → rank, any positive numbers
 * @param {string} id
 * @returns {Record<string, number>} id → rank, 1..n, `id` highest
 */
export function raisedOrder(ranks, id) {
  const ids = Object.keys(ranks)
  if (!ids.includes(id)) return { ...ranks }
  const ordered = ids
    .filter((other) => other !== id)
    .sort((a, b) => ranks[a] - ranks[b])
  ordered.push(id)
  /** @type {Record<string, number>} */
  const next = {}
  ordered.forEach((other, index) => {
    next[other] = index + 1
  })
  return next
}
