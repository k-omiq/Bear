/**
 * The undo journal, as arithmetic - one list of deltas, partitioned by page.
 *
 * This is the JavaScript half of `src-tauri/src/history.rs`, and the two are
 * deliberately the same operations with the same four rules:
 *
 *  1. **Undo and redo never leave the page.** Every entry belongs to exactly
 *     one page (`page`), and an entry on the redo side is marked (`undone`).
 *     Undo on a page marks that page's newest entry that is not undone; redo
 *     clears the mark of its oldest undone one. Within a page the undone
 *     entries are always its newest. Entries of other pages are never
 *     answered and never changed. An entry whose regions are on several pages
 *     is kept as one entry per page.
 *  2. A push **drops the future of its own page**. Once a new action happens
 *     on a page its old redo side is unreachable, which is the rule the
 *     session-only history had and the one a persisted history is most likely
 *     to lose. Every other page's redo side stays.
 *  3. The journal is **capped**, oldest out. An unlimited persisted history is
 *     an unbounded term in a budget whose whole claim is that it is bounded.
 *     The cap is the `undoLimit` setting and the backend owns it: there is no
 *     number here, the caller passes the limit in. A page whose undone entry
 *     is trimmed loses its whole redo side.
 *  4. `seq` is **never reissued**. A number that came back would make two
 *     different edits the same entry to anything holding the old one.
 *
 * `cursor` is kept as the count of entries not undone, as the file keeps it;
 * nothing steers by it.
 *
 * Two implementations rather than one because they run in two processes: the
 * core owns the file, and the mock backend owns a fixture library that has no
 * file. `journal.test.js` is written against this one and
 * `history::tests` against the other, and both assert the rules above.
 *
 * Nothing here is reactive and nothing here touches the seam - a journal is
 * plain data, and `src/lib/model/history.js` is the interface's index of it.
 */

/** The format `src-tauri/src/history.rs` writes. */
export const JOURNAL_VERSION = 2

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
 * @property {'region-state'|'clean-state'|'detection-state'} op - the verb. `region-state` is an
 *   edit the interface recorded: each side is the region as it was, and a
 *   replay puts that side back (`restoreRegion`). `clean-state` is a clean of
 *   a detected region, or a rejected result, which the backend records by
 *   itself: one side's region is the detection (`outcome: 'detected'`), the
 *   other its cleaned result, and a replay asks for whichever the side is
 *   (`setRegionCleaned`), so nothing renders again
 * @property {string} regionId
 * @property {DeltaSide} before
 * @property {DeltaSide} after
 * @property {Array<{regionId: string, before: DeltaSide, after: DeltaSide}>} [parts]
 * @property {number|null} [page] - the 0-based page index the entry is a step of
 * @property {boolean} [undone] - whether it is on its page's redo side
 */

/**
 * @typedef {Object} Journal
 * @property {number} version
 * @property {number} cursor - how many entries are not undone; nothing steers by it
 * @property {number} nextSeq
 * @property {Delta[]} entries
 */

/** @returns {Journal} */
export function createJournal() {
  return { version: JOURNAL_VERSION, cursor: 0, nextSeq: 1, entries: [] }
}

/**
 * Record an edit that has **already happened**, on its page: that page's
 * undone entries go, every other page's stay. An entry whose `parts` are on
 * several pages is kept as one entry per page (`piecesOf`).
 *
 * @param {Journal} journal
 * @param {Omit<Delta, 'seq'>} delta
 * @param {number} limit - how many entries the journal keeps
 * @param {(regionId: string) => number|null} [pageOf] - the page a region is on; `delta.page` when it cannot say
 * @returns {Delta} the entry, or its first piece, with the number it was given
 */
export function pushEntry(journal, delta, limit, pageOf = () => null) {
  let first = null
  for (const piece of piecesOf(delta, pageOf)) {
    const entry = { ...piece, seq: journal.nextSeq, undone: false }
    journal.nextSeq += 1
    journal.entries = journal.entries.filter((held) => held.page !== entry.page || !held.undone)
    journal.entries.push(entry)
    first ??= entry
  }
  const excess = Math.max(0, journal.entries.length - limit)
  const cut = new Set(journal.entries.slice(0, excess).filter((held) => held.undone).map((held) => held.page))
  journal.entries = journal.entries.slice(excess).filter((held) => !(held.undone && cut.has(held.page)))
  syncCursor(journal)
  return /** @type {Delta} */ (first)
}

/**
 * An entry as the journal keeps it, one per page, as `history.rs#pieces`
 * splits it: each page's parts, in the order the pages first appear, and
 * each page's first part also the entry's own region and sides. A piece of
 * one region has no `parts`. A `detection-state` entry is left whole; its
 * page is the caller's to say.
 *
 * @param {Omit<Delta, 'seq'>} delta
 * @param {(regionId: string) => number|null} pageOf
 * @returns {Array<Omit<Delta, 'seq'>>}
 */
export function piecesOf(delta, pageOf) {
  const own = (regionId) => {
    const page = pageOf(regionId)
    return Number.isInteger(page) ? page : Number.isInteger(delta.page) ? delta.page : null
  }
  if (delta.op === 'detection-state') return [{ ...delta, page: Number.isInteger(delta.page) ? delta.page : null }]
  if (!delta.parts?.length) return [{ ...delta, page: own(delta.regionId) }]
  /** @type {Map<number|null, any[]>} */
  const pages = new Map()
  for (const part of delta.parts) {
    const page = own(part.regionId)
    if (!pages.has(page)) pages.set(page, [])
    pages.get(page)?.push(part)
  }
  if (pages.size === 1) return [{ ...delta, page: [...pages.keys()][0] }]
  return [...pages].map(([page, parts]) => {
    const { parts: _all, ...rest } = delta
    return {
      ...rest,
      regionId: parts[0].regionId,
      before: parts[0].before,
      after: parts[0].after,
      ...(parts.length > 1 ? { parts } : {}),
      page,
    }
  })
}

/**
 * The entry a step in `direction` on `page` would answer with, nothing marked.
 *
 * @param {Journal} journal
 * @param {number} page
 * @param {'undo'|'redo'} direction
 * @returns {Delta|null}
 */
export function nextEntry(journal, page, direction) {
  if (direction === 'undo') {
    for (let at = journal.entries.length - 1; at >= 0; at--) {
      const entry = journal.entries[at]
      if (entry.page === page && !entry.undone) return entry
    }
    return null
  }
  if (direction === 'redo') return journal.entries.find((entry) => entry.page === page && entry.undone) ?? null
  return null
}

/**
 * Mark the newest entry of `page` that is not undone, and answer with it.
 *
 * @param {Journal} journal
 * @param {number} page
 * @returns {Delta|null}
 */
export function undoEntry(journal, page) {
  const entry = nextEntry(journal, page, 'undo')
  if (!entry) return null
  entry.undone = true
  syncCursor(journal)
  return entry
}

/**
 * Clear the mark of the oldest undone entry of `page`, and answer with it.
 *
 * @param {Journal} journal
 * @param {number} page
 * @returns {Delta|null}
 */
export function redoEntry(journal, page) {
  const entry = nextEntry(journal, page, 'redo')
  if (!entry) return null
  entry.undone = false
  syncCursor(journal)
  return entry
}

/** @param {Journal} journal */
function syncCursor(journal) {
  journal.cursor = journal.entries.filter((entry) => !entry.undone).length
}

/**
 * The index the interface holds: one `{seq, label, page, undone}` per entry.
 * **No payload** - that is the whole of what persisted undo costs in RAM, and
 * it is flat in the size of the edits rather than in their number times their
 * size.
 *
 * @param {Journal} journal
 * @param {number} limit - the limit the journal is kept to, reported with the index
 * @returns {{limit: number, cursor: number, entries: Array<{seq: number, label: string, page: number|null, undone: boolean}>}}
 */
export function viewOf(journal, limit) {
  return {
    limit,
    cursor: journal.cursor,
    entries: journal.entries.map((entry) => ({
      seq: entry.seq,
      label: entry.label,
      page: Number.isInteger(entry.page) ? /** @type {number} */ (entry.page) : null,
      undone: !!entry.undone,
    })),
  }
}
