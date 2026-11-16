/**
 * Undo/redo, one index per chapter, **persisted to the project folder** and
 * **partitioned by page**: Undo and Redo never leave the page on screen. Undo
 * on a page takes back only that page's newest step, Redo repeats only that
 * page's steps, and a new edit on a page drops only that page's redo side. A
 * chapter-wide stack walked back through edits on pages the user could not
 * see, and the next edit cut that work off. Plain data plus free functions
 * rather than a class, so a `History` can live directly inside a component's
 * `$state` - mutating the arrays in place is what makes that reactive
 * without extra plumbing.
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
 * So a history is now an **index**: one `{seq, label, page, undone}` record
 * per entry of the chapter's journal, oldest first, and nothing else. An entry
 * belongs to one page, and `undone` puts it on that page's redo side; within a
 * page the undone entries are always its newest. That is enough to answer
 * `canUndo`, `canRedo` and both tooltips for any page, which is everything the
 * interface reads. The payload - the delta that says what to reverse - lives
 * on disk and is fetched one entry at a time, at the moment it is replayed.
 * RAM is flat in edit count.
 *
 * ## The marks still move synchronously
 *
 * `canUndo` and the undo label must never lag behind the click that changed
 * them, so the mark moves at call time and the backend round trip is queued
 * behind whatever is already in flight. `running` is the tail of that chain:
 * each replay is queued behind the one before it, and a rejection is caught
 * there - the mark has already moved, and an unhandled rejection on top of
 * that helps nobody. The failure is kept on `error` for a caller that wants to
 * say so. `revision` counts the local moves, so an index read while one was
 * made can be told to be older than it.
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
 * @property {number|null} [page] - the 0-based page index the entry is a step of
 * @property {boolean} [undone] - whether it is on its page's redo side
 */

/**
 * @typedef {Object} HistoryEntry
 * @property {number} seq
 * @property {string} label
 * @property {number|null} page
 * @property {boolean} undone
 */

/**
 * @typedef {Object} History
 * @property {HistoryEntry[]} entries - the whole journal, oldest first, every page's
 * @property {number} revision - how many local pushes and moves were made, to tell a stale read
 * @property {number} limit - how many entries the backend keeps; unbounded until it has said
 * @property {Promise<void>} running - the tail of the task chain; await it to settle
 * @property {number} pending - how many tasks are queued or running
 * @property {unknown} error - why the last task failed, or null
 */

/**
 * @returns {History}
 */
export function createHistory() {
  return { entries: [], revision: 0, limit: Infinity, running: Promise.resolve(), pending: 0, error: null }
}

/**
 * Take on the journal a backend just reported - on opening a chapter, and after
 * every write, since the backend is the one that numbers entries, gives them
 * their pages and applies the cap. The cap itself comes with the index
 * (`limit`, the `undoLimit` setting as the backend reads it), so this side
 * keeps no copy of the number. An index from a backend that sends no marks
 * is read by its `cursor`, as one stack.
 *
 * @param {History} history
 * @param {{limit?: number, cursor?: number, entries?: HistoryLabel[]}|null|undefined} view
 * @returns {History}
 */
export function adopt(history, view) {
  const entries = Array.isArray(view?.entries) ? view.entries : []
  const cursor = clampCursor(view?.cursor, entries.length)
  history.entries = entries.map((entry, at) => ({
    seq: entry.seq,
    label: entry.label,
    page: Number.isInteger(entry.page) ? /** @type {number} */ (entry.page) : null,
    undone: typeof entry.undone === 'boolean' ? entry.undone : at >= cursor,
  }))
  if (Number.isInteger(view?.limit) && view.limit > 0) history.limit = view.limit
  return history
}

/**
 * Records a just-performed command on its page, locally. Clears that page's
 * redo side - once a new action happens on a page, its old future is no
 * longer reachable - and leaves every other page's alone. Applies the cap the
 * backend last reported, so the index cannot describe entries the file has
 * already dropped, and a page whose undone entry is trimmed loses its whole
 * redo side, as the journal does.
 *
 * @param {History} history
 * @param {{seq: number, label: string, page: number|null}} entry
 * @returns {History}
 */
export function push(history, entry) {
  const page = Number.isInteger(entry.page) ? entry.page : null
  const kept = history.entries.filter((held) => held.page !== page || !held.undone)
  kept.push({ seq: entry.seq, label: entry.label, page, undone: false })
  const excess = Math.max(0, kept.length - history.limit)
  const cut = new Set(kept.slice(0, excess).filter((held) => held.undone).map((held) => held.page))
  history.entries = kept.slice(excess).filter((held) => !(held.undone && cut.has(held.page)))
  history.revision = (history.revision ?? 0) + 1
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
 * Step back one entry **of `page`**: its newest that is not undone is marked
 * undone. Entries of other pages are never touched. `replay` is handed the
 * direction, the page and the entry, and does the backend round trip: mark
 * the journal's entry on disk, take the delta it answers with, apply it.
 *
 * @param {History} history
 * @param {number} page - the page on screen
 * @param {(direction: 'undo'|'redo', page: number, entry: HistoryEntry) => void|Promise<void>} replay
 * @returns {History}
 */
export function undo(history, page, replay) {
  const entry = newestDone(history, page)
  if (!entry) return history
  entry.undone = true
  history.revision = (history.revision ?? 0) + 1
  enqueue(history, () => replay('undo', page, entry))
  return history
}

/**
 * Repeat the oldest undone entry **of `page`**.
 *
 * @param {History} history
 * @param {number} page - the page on screen
 * @param {(direction: 'undo'|'redo', page: number, entry: HistoryEntry) => void|Promise<void>} replay
 * @returns {History}
 */
export function redo(history, page, replay) {
  const entry = oldestUndone(history, page)
  if (!entry) return history
  entry.undone = false
  history.revision = (history.revision ?? 0) + 1
  enqueue(history, () => replay('redo', page, entry))
  return history
}

/**
 * @param {History} history
 * @param {number|null|undefined} page
 * @returns {HistoryEntry|null}
 */
function newestDone(history, page) {
  if (!Number.isInteger(page)) return null
  for (let at = history.entries.length - 1; at >= 0; at--) {
    const entry = history.entries[at]
    if (entry.page === page && !entry.undone) return entry
  }
  return null
}

/**
 * @param {History} history
 * @param {number|null|undefined} page
 * @returns {HistoryEntry|null}
 */
function oldestUndone(history, page) {
  if (!Number.isInteger(page)) return null
  return history.entries.find((entry) => entry.page === page && entry.undone) ?? null
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
 * @param {number|null|undefined} page - the page on screen
 * @returns {boolean}
 */
export function canUndo(history, page) {
  return newestDone(history, page) !== null
}

/**
 * @param {History} history
 * @param {number|null|undefined} page - the page on screen
 * @returns {boolean}
 */
export function canRedo(history, page) {
  return oldestUndone(history, page) !== null
}

/**
 * The i18n key of the action Undo on `page` would reverse.
 * @param {History} history
 * @param {number|null|undefined} page
 * @returns {string|null}
 */
export function undoLabel(history, page) {
  return newestDone(history, page)?.label ?? null
}

/**
 * The i18n key of the action Redo on `page` would repeat.
 * @param {History} history
 * @param {number|null|undefined} page
 * @returns {string|null}
 */
export function redoLabel(history, page) {
  return oldestUndone(history, page)?.label ?? null
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
