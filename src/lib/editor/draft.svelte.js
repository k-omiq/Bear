/**
 * The gesture in progress: what the pointer (or the arrow keys) have described
 * so far, before anything has been committed through the seam.
 *
 * **State only.** This module imports nothing - not the editor's state, not the
 * backend, not Svelte's DOM. That is what lets `state/editor.svelte.js` clear a
 * draft from `cancelInteraction()` and `resetSessionState()` without a cycle,
 * and it keeps "what has been drawn" separate from "what has been applied": a
 * draft is not an edit, and abandoning one must leave the chapter untouched.
 *
 * The clone stamp's source lives here too. It is not part of any one gesture -
 * it is sampled once and read by every stroke afterwards - but it is the same
 * kind of thing: a decision the user has made with a tool that no backend
 * knows about yet.
 */

/**
 * @typedef {Object} Draft
 * @property {string} tool - which tool is drawing
 * @property {'rect'|'ellipse'|'lasso'|'polygon'|'stroke'} kind - what to draw as a preview
 * @property {string} pageId - the page it is being drawn on; a draft belongs to one page
 * @property {Array<{x: number, y: number, p?: number}>} points - in page percent
 * @property {import('./gesture.js').Bbox|null} bbox
 * @property {'add'|'erase'|'paint'|'detect'} mode - `detect` is the selection tool's Detect of a drawn area
 * @property {boolean} keyboard - opened with the keyboard, so it is adjustable and waits for Enter
 * @property {boolean} moved - the pointer travelled far enough to be a drag rather than a tap
 * @property {{source: {x: number, y: number}, offset: {x: number, y: number}}} [clone]
 *   where a clone stamp stroke reads from, fixed when the stroke starts (`gesture.js#cloneOffset`)
 * @property {boolean} [committed] - sent through the seam rather than abandoned
 */

/**
 * A Patch or Gradient selection that has been drawn and not yet applied
 * (`retouch.svelte.js`). The gesture is over, so it is not `active`; nothing
 * has crossed the seam, so it is not an edit either.
 *
 * @typedef {Object} HeldSelection
 * @property {'patch'|'gradient'} tool
 * @property {string} pageId
 * @property {string|null} chapterId
 * @property {number|null} pageIndex
 * @property {number|null} sourceIndex
 * @property {string|null} sourceSha
 * @property {import('./gesture.js').Bbox} bbox - page percent
 * @property {{kind: string, points: Array<{x: number, y: number}>, feather: number}|null} painted - the area, as `drawing.svelte.js#paintedShape` sends it
 * @property {{points: Array<{x: number, y: number}>, radius: number}|null} stroke - a brush selection's path instead
 * @property {{x: number, y: number}|null} offset - Patch: to the source (or the destination), page percent
 * @property {boolean} proposed - Patch: the offset is the proposal, not a drag
 * @property {boolean} proposing - Patch: the proposal has been asked for and has not come back
 * @property {{x: number, y: number}|null} start - Gradient: the line, page percent
 * @property {{x: number, y: number}|null} end
 * @property {string|null} regionId - Gradient: the layer being edited again, or null for a new one
 */

export const draft = $state({
  /** @type {Draft|null} */
  active: null,
  /** @type {HeldSelection|null} */
  selection: null,
  /** @type {{pageId: string, x: number, y: number}|null} the clone stamp's sampled source */
  cloneSource: null,
  /** @type {{x: number, y: number}|null} the aligned offset, once a stroke has established it */
  cloneOffset: null,
  /** @type {{pageId: string, x: number, y: number}|null} where the last clone stroke ended */
  cloneLast: null,
})

/**
 * The drawing surface of each mounted page, by page id, for the one gesture
 * that is made of separate clicks: a polygon. In a longstrip a polygon
 * started on one page goes on across the join, and its next click lands on
 * the next page's surface, which has no draft of its own. That surface hands
 * the event to the page the draft belongs to (`forwardToDraft`), which reads
 * it against its own sheet, so the polygon stays one draft on one page.
 *
 * Plain, not state: nothing draws from it.
 *
 * @type {Map<string, (kind: 'vertex'|'move'|'close', event: PointerEvent|MouseEvent) => void>}
 */
const hosts = new Map()

/**
 * @param {string} pageId
 * @param {(kind: 'vertex'|'move'|'close', event: PointerEvent|MouseEvent) => void} handler
 * @returns {() => void} what unregisters it
 */
export function registerDraftHost(pageId, handler) {
  hosts.set(pageId, handler)
  return () => {
    if (hosts.get(pageId) === handler) hosts.delete(pageId)
  }
}

/**
 * Hand a click, a move or a double-click to the page the active draft is on.
 *
 * @param {'vertex'|'move'|'close'} kind
 * @param {PointerEvent|MouseEvent} event
 * @returns {boolean} whether a host took it
 */
export function forwardToDraft(kind, event) {
  const handler = draft.active ? hosts.get(draft.active.pageId) : undefined
  if (!handler) return false
  handler(kind, event)
  return true
}

/**
 * @param {Draft} spec
 * @returns {Draft}
 */
export function beginDraft(spec) {
  draft.active = spec
  return spec
}

/** @param {{x: number, y: number}} point */
export function addPoint(point) {
  draft.active?.points.push(point)
}

/** @param {import('./gesture.js').Bbox|null} bbox */
export function setDraftBbox(bbox) {
  if (draft.active) draft.active.bbox = bbox
}

/** The pointer has travelled far enough that this is a drag, not a tap. */
export function markMoved() {
  if (draft.active) draft.active.moved = true
}

/**
 * The draft is about to go through the seam. `clearDraft` cannot say whether a
 * gesture was committed or abandoned - both leave `active` null - and the
 * paint preview has to know, because a committed stroke's pixels are on their
 * way and an abandoned one's are not (`PaintLayer.svelte`).
 */
export function markCommitted() {
  if (draft.active) draft.active.committed = true
}

/** @returns {boolean} whether there was anything to drop */
export function clearDraft() {
  if (!draft.active) return false
  draft.active = null
  return true
}

/**
 * Forget the sampled clone source.
 *
 * Until this existed, re-sampling was the only way to change it: `clearDraft`
 * drops the stroke and leaves the source, and `resetDraftState` only runs when
 * the chapter changes. A source aimed at the wrong place was therefore
 * effectively permanent, which is not a state a tool should be able to get
 * stuck in. `cancelInteraction` gives it a rung on the Escape ladder.
 *
 * @returns {boolean} whether there was a source to drop
 */
export function clearCloneSource() {
  if (!draft.cloneSource) return false
  draft.cloneSource = null
  draft.cloneOffset = null
  return true
}

/**
 * Hold a drawn Patch or Gradient selection until Apply.
 *
 * @param {HeldSelection} selection
 */
export function holdSelection(selection) {
  draft.selection = selection
  return selection
}

/** @returns {boolean} whether there was a held selection to drop */
export function clearSelection() {
  if (!draft.selection) return false
  draft.selection = null
  return true
}

/**
 * Drop everything, including the sampled clone source - a new chapter is a new
 * page under the stamp.
 */
export function resetDraftState() {
  draft.active = null
  draft.selection = null
  draft.cloneSource = null
  draft.cloneOffset = null
  draft.cloneLast = null
}

/**
 * @param {string} pageId
 * @param {{x: number, y: number}} point
 */
export function setCloneSource(pageId, point) {
  draft.cloneSource = { pageId, x: point.x, y: point.y }
  // A freshly sampled source has no established offset yet; the next stroke
  // establishes it, and `aligned` is what then holds on to it.
  draft.cloneOffset = null
}

/**
 * A clone stroke has been made: keep the offset it read at, which `aligned`
 * holds for the strokes after it, and where it ended, which is where a
 * Shift-click draws its straight line from.
 *
 * @param {string} pageId
 * @param {{x: number, y: number}} offset
 * @param {{x: number, y: number}} end
 */
export function setCloneStroke(pageId, offset, end) {
  draft.cloneOffset = { x: offset.x, y: offset.y }
  draft.cloneLast = { pageId, x: end.x, y: end.y }
}
