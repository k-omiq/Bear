/**
 * The undo journal, as arithmetic - one cursor over one list of deltas.
 *
 * This is the JavaScript half of `src-tauri/src/history.rs`, and the two are
 * deliberately the same four operations with the same three rules:
 *
 *  1. A push **truncates the future**. Once a new action happens the old redo
 *     stack is unreachable, which is the rule the session-only history had and
 *     the one a persisted history is most likely to lose.
 *  2. The journal is **capped**, oldest out, and the cursor moves with the
 *     entries that leave. An unlimited persisted history is an unbounded term
 *     in a budget whose whole claim is that it is bounded. The cap is the
 *     `undoLimit` setting and the backend owns it: there is no number here,
 *     the caller passes the limit in.
 *  3. `seq` is **never reissued**. A number that came back would make two
 *     different edits the same entry to anything holding the old one.
 *
 * Two implementations rather than one because they run in two processes: the
 * core owns the file, and the mock backend owns a fixture library that has no
 * file. `journal.test.js` is written against this one and
 * `history::tests` against the other, and both assert the three rules above.
 *
 * Nothing here is reactive and nothing here touches the seam - a journal is
 * plain data, and `src/lib/state/history.svelte.js` is what makes it move.
 */

/** The format `src-tauri/src/history.rs` writes. */
export const JOURNAL_VERSION = 1

/**
 * @typedef {Object} DeltaSide
 * @property {boolean} present - whether the region existed at all in this state
 * @property {string|null} [pageStatus] - the page's status in this state
 * @property {Object|null} [region] - the region's metadata, never pixels
 */

/**
 * @typedef {Object} Delta
 * @property {number} seq
 * @property {string} label - i18n key for the Undo/Redo tooltip
 * @property {'region-state'|'clean-state'} op - the verb. `region-state` is an
 *   edit the interface recorded: each side is the region as it was, and a
 *   replay puts that side back (`restoreRegion`). `clean-state` is a clean of
 *   a detected region, or a rejected result, which the backend records by
 *   itself: one side's region is the detection (`outcome: 'detected'`), the
 *   other its cleaned result, and a replay asks for whichever the side is
 *   (`setRegionCleaned`), so nothing renders again
 * @property {string} regionId
 * @property {DeltaSide} before
 * @property {DeltaSide} after
 */

/**
 * @typedef {Object} Journal
 * @property {number} version
 * @property {number} cursor - how many entries are in the past
 * @property {number} nextSeq
 * @property {Delta[]} entries
 */

/** @returns {Journal} */
export function createJournal() {
  return { version: JOURNAL_VERSION, cursor: 0, nextSeq: 1, entries: [] }
}

/**
 * Record an edit that has **already happened**.
 *
 * @param {Journal} journal
 * @param {Omit<Delta, 'seq'>} delta
 * @param {number} limit - how many entries the journal keeps
 * @returns {Delta} the entry, with the number it was given
 */
export function pushEntry(journal, delta, limit) {
  journal.entries.length = journal.cursor
  const entry = { ...delta, seq: journal.nextSeq }
  journal.nextSeq += 1
  journal.entries.push(entry)
  journal.entries.splice(0, Math.max(0, journal.entries.length - limit))
  journal.cursor = journal.entries.length
  return entry
}

/**
 * @param {Journal} journal
 * @returns {Delta|null}
 */
export function undoEntry(journal) {
  if (journal.cursor === 0) return null
  journal.cursor -= 1
  return journal.entries[journal.cursor] ?? null
}

/**
 * @param {Journal} journal
 * @returns {Delta|null}
 */
export function redoEntry(journal) {
  const entry = journal.entries[journal.cursor]
  if (!entry) return null
  journal.cursor += 1
  return entry
}

/**
 * The index the interface holds: a cursor and one `{seq, label}` per entry.
 * **No payload** - that is the whole of what persisted undo costs in RAM, and
 * it is flat in the size of the edits rather than in their number times their
 * size.
 *
 * @param {Journal} journal
 * @param {number} limit - the limit the journal is kept to, reported with the index
 * @returns {{limit: number, cursor: number, entries: Array<{seq: number, label: string}>}}
 */
export function viewOf(journal, limit) {
  return {
    limit,
    cursor: journal.cursor,
    entries: journal.entries.map((entry) => ({ seq: entry.seq, label: entry.label })),
  }
}
