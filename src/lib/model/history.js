/**
 * Global undo/redo: one stack across every tool, **persisted
 * to the project folder**. Plain data plus free
 * functions rather than a class, so a `History` can live directly inside a
 * component's `$state` - mutating the arrays in place is what makes that
 * reactive without extra plumbing.
 *
 * ## What changed, and why the shape did
 *
 * A history used to be two stacks of `{label, undo(), redo()}` closures. Two
 * things were wrong with that, and only one of them was the forgetting:
 *
 *  - **A closure keeps its page alive.** Each command closed over two whole
 *    region snapshots, so a session's history pinned every page the user had
 *    edited - which is exactly what the chapter-wide residency's three-page
 *    window exists to remove. Evicting page 140 gave back nothing while a
 *    closure over page 140 was still on the stack.
 *  - **A closure cannot be written down**, so nothing survived a restart.
 *
 * So a history is now an **index**: a cursor over `{seq, label}` pairs, and
 * nothing else. That is enough to answer `canUndo`, `canRedo` and both
 * tooltips, which is everything the interface reads. The payload - the delta
 * that says what to reverse - lives on disk and is fetched one entry at a time,
 * at the moment it is replayed. RAM is flat in edit count.
 *
 * ## The stacks still move synchronously
 *
 * `canUndo` and the undo label must never lag behind the click that changed
 * them, so the cursor moves at call time and the backend round trip is queued
 * behind whatever is already in flight. `running` is the tail of that chain:
 * each replay is queued behind the one before it, and a rejection is caught
 * there - the cursor has already moved, and an unhandled rejection on top of
 * that helps nobody. The failure is kept on `error` for a caller that wants to
 * say so.
 *
 * Two fast undos would otherwise put two adapter calls in flight with no
 * ordering guarantee, and whichever settled last would win.
 *
 * **One queue for everything.** Pushes, replays and index reads all go
 * through `enqueue`, and `pending` counts what is queued. A second queue that
 * started a task at once whenever it believed itself idle could start a push
 * while an undo was still waiting its turn on this one, and the two would
 * reach the journal in either order.
 */

/**
 * @typedef {Object} HistoryLabel
 * @property {number} seq - the entry's number in the chapter's journal
 * @property {string} label - i18n key describing the action, for the tooltip
 */

/**
 * @typedef {Object} History
 * @property {HistoryLabel[]} entries - the whole journal, oldest first
 * @property {number} cursor - how many entries are in the past
 * @property {number} limit - how many entries the backend keeps; unbounded until it has said
 * @property {Promise<void>} running - the tail of the task chain; await it to settle
 * @property {number} pending - how many tasks are queued or running
 * @property {unknown} error - why the last task failed, or null
 */

/**
 * @returns {History}
 */
export function createHistory() {
  return { entries: [], cursor: 0, limit: Infinity, running: Promise.resolve(), pending: 0, error: null }
}

/**
 * Take on the journal a backend just reported - on opening a chapter, and after
 * every write, since the backend is the one that numbers entries and applies
 * the cap. The cap itself comes with the index (`limit`, the `undoLimit`
 * setting as the backend reads it), so this side keeps no copy of the number.
 *
 * @param {History} history
 * @param {{limit?: number, cursor?: number, entries?: HistoryLabel[]}|null|undefined} view
 * @returns {History}
 */
export function adopt(history, view) {
  const entries = Array.isArray(view?.entries) ? view.entries : []
  history.entries = entries.map((entry) => ({ seq: entry.seq, label: entry.label }))
  history.cursor = clampCursor(view?.cursor, history.entries.length)
  if (Number.isInteger(view?.limit) && view.limit > 0) history.limit = view.limit
  return history
}

/**
 * Records a just-performed command, locally. Clears the redo stack - once a new
 * action happens, the old future is no longer reachable - and applies the cap
 * the backend last reported, so the index cannot describe entries the file has
 * already dropped.
 *
 * @param {History} history
 * @param {HistoryLabel} entry
 * @returns {History}
 */
export function push(history, entry) {
  history.entries.length = history.cursor
  history.entries.push({ seq: entry.seq, label: entry.label })
  history.entries.splice(0, Math.max(0, history.entries.length - history.limit))
  history.cursor = history.entries.length
  return history
}

/**
 * Queue one task behind everything already in flight: a replay, a push, a
 * read of the index. A rejection is kept on `error` and does not stop the
 * tasks behind it.
 *
 * @param {History} history
 * @param {() => void|Promise<void>} run
 */
export function enqueue(history, run) {
  // An idle queue starts the task now, in the caller's turn: nothing is
  // ahead of it, and a push that waited a tick would let a click land first.
  const idle = !busy(history)
  history.pending = (history.pending ?? 0) + 1
  history.running = (idle ? started(run) : history.running.then(run))
    .then(
      () => undefined,
      (error) => {
        history.error = error
      },
    )
    .finally(() => {
      history.pending -= 1
    })
}

/**
 * @param {() => void|Promise<void>} run
 * @returns {Promise<void>}
 */
function started(run) {
  try {
    return Promise.resolve(run())
  } catch (error) {
    return Promise.reject(error)
  }
}

/**
 * Whether a task is queued or running, so an index a backend answered with
 * may already be older than what those leave behind.
 *
 * @param {History} history
 * @returns {boolean}
 */
export function busy(history) {
  return (history.pending ?? 0) > 0
}

/**
 * Step back one entry. `replay` is handed the direction and does the backend
 * round trip: move the journal's cursor on disk, take the delta it answers
 * with, apply it.
 *
 * @param {History} history
 * @param {(direction: 'undo'|'redo') => void|Promise<void>} replay
 * @returns {History}
 */
export function undo(history, replay) {
  if (history.cursor === 0) return history
  history.cursor -= 1
  enqueue(history, () => replay('undo'))
  return history
}

/**
 * @param {History} history
 * @param {(direction: 'undo'|'redo') => void|Promise<void>} replay
 * @returns {History}
 */
export function redo(history, replay) {
  if (history.cursor >= history.entries.length) return history
  history.cursor += 1
  enqueue(history, () => replay('redo'))
  return history
}

/**
 * Resolves once every queued replay has run. For tests, and for a caller that
 * needs to know the backend has caught up before it reads it.
 *
 * @param {History} history
 * @returns {Promise<void>}
 */
export function settled(history) {
  return history.running ?? Promise.resolve()
}

/**
 * @param {History} history
 * @returns {boolean}
 */
export function canUndo(history) {
  return history.cursor > 0
}

/**
 * @param {History} history
 * @returns {boolean}
 */
export function canRedo(history) {
  return history.cursor < history.entries.length
}

/**
 * The i18n key of the action Undo would reverse.
 * @param {History} history
 * @returns {string|null}
 */
export function undoLabel(history) {
  return history.entries[history.cursor - 1]?.label ?? null
}

/**
 * The i18n key of the action Redo would repeat.
 * @param {History} history
 * @returns {string|null}
 */
export function redoLabel(history) {
  return history.entries[history.cursor]?.label ?? null
}

/**
 * @param {unknown} value
 * @param {number} length
 * @returns {number}
 */
function clampCursor(value, length) {
  const cursor = Number(value)
  if (!Number.isFinite(cursor)) return length
  return Math.min(length, Math.max(0, Math.trunc(cursor)))
}
