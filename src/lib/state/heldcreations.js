/**
 * Region creations whose undo entry is held back while a cloud render runs.
 *
 * A cloud stroke is one undoable gesture (M1). Its local seed is stored first,
 * because consent binds to a stored region, and the one creation entry is
 * recorded when the render settles, with the rendered region as its after
 * side (`editor/drawing.svelte.js`). Until then the seed is on the page with
 * nothing in the history behind it.
 *
 * Any other edit to the seed in that window - a Delete, a Try again, a tool
 * click - is a gesture of its own and belongs *after* the creation it edits.
 * `recordRegionEdit` settles the hold first: the creation is recorded as the
 * seed, then the edit. Undoing a Delete brings the seed back and one more undo
 * removes it, where before the Delete was the only entry and the seed could
 * not be undone at all.
 *
 * The render's own entry is recognised by its `before` object, the one the
 * hold was taken with, and only releases the hold.
 *
 * No imports, so the editor state and the drawing tools can both use it
 * without importing each other.
 */

/** @type {Map<string, {before: object, record: () => void}>} */
const held = new Map()

/**
 * Hold a creation's undo entry until the render settles or the seed is edited.
 *
 * @param {string} regionId
 * @param {object} before - the `before` the render will record with, by identity
 * @param {() => void} record - records the creation as it stands now, the seed
 */
export function holdCreation(regionId, before, record) {
  held.set(regionId, { before, record })
}

/**
 * Called before an edit to `regionId` is recorded. An edit with the hold's own
 * `before` is the creation itself landing, and only releases the hold. Any
 * other edit records the held creation first, so it sits before the edit.
 *
 * @param {string} regionId
 * @param {unknown} before - the edit's `before`
 */
export function settleCreation(regionId, before) {
  const pending = held.get(regionId)
  if (!pending) return
  held.delete(regionId)
  if (pending.before !== before) pending.record()
}

/**
 * Drop the hold once the render has settled.
 *
 * @param {string} regionId
 * @param {object} before - the hold's own `before`, so a later hold on the same id is left alone
 * @returns {boolean} true when the hold was still waiting, so nothing has recorded the creation yet
 */
export function releaseCreation(regionId, before) {
  const pending = held.get(regionId)
  if (!pending || pending.before !== before) return false
  held.delete(regionId)
  return true
}
