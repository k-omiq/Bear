import { describe, expect, it } from 'vitest'
import {
  createJournal,
  nextEntry,
  pushEntry,
  redoEntry,
  undoEntry,
  viewOf,
} from './journal.js'

/*
 * These are the same assertions `history::tests` makes in Rust, against the
 * same four rules. Two implementations run in two processes - the core owns a
 * file, the mock owns a fixture library with no file - and the point of writing
 * the tests twice is that the rules are the contract, not the storage.
 */

/** The limit is the backend's to choose; these are its lowest, default and highest. */
const LIMITS = [20, 500, 2000]
const LIMIT = 500

/** Region ids say their page here as the app's do: `p<page>-...`. */
const pageOf = (regionId) => {
  const found = /^p(\d+)-/.exec(regionId)
  return found ? Number(found[1]) : null
}
const on = (page, n = 1) => `p${page}-r${n}`

const delta = (label, regionId = on(0)) => ({
  label,
  op: 'region-state',
  regionId,
  before: { present: true, pageStatus: 'cleaned', region: { id: regionId } },
  after: { present: false, pageStatus: 'unclean', region: null },
})
const push = (j, label, regionId, limit = LIMIT) => pushEntry(j, delta(label, regionId), limit, pageOf)
const marks = (j) => j.entries.map((e) => [e.label, e.page, e.undone])

describe('journal', () => {
  it('numbers an entry and puts the cursor at the end', () => {
    const j = createJournal()
    expect(push(j, 'a').seq).toBe(1)
    expect(push(j, 'b').seq).toBe(2)
    expect(j.cursor).toBe(2)
  })

  it('the view carries labels, pages and marks, and no payload', () => {
    const j = createJournal()
    push(j, 'masks.command.deleteMask', on(3))
    const view = viewOf(j, LIMIT)
    expect(view).toEqual({ limit: LIMIT, cursor: 1, entries: [{ seq: 1, label: 'masks.command.deleteMask', page: 3, undone: false }] })
    expect(JSON.stringify(view)).not.toContain('region-state')
  })

  it('undo and redo walk the same entries', () => {
    const j = createJournal()
    push(j, 'a')
    push(j, 'b')

    expect(undoEntry(j, 0).label).toBe('b')
    expect(undoEntry(j, 0).label).toBe('a')
    expect(undoEntry(j, 0)).toBe(null)
    expect(j.cursor).toBe(0)

    expect(redoEntry(j, 0).label).toBe('a')
    expect(redoEntry(j, 0).label).toBe('b')
    expect(redoEntry(j, 0)).toBe(null)
  })

  it('a push after an undo drops the future', () => {
    const j = createJournal()
    push(j, 'a')
    push(j, 'b')
    undoEntry(j, 0)
    push(j, 'c')

    expect(j.entries.map((e) => e.label)).toEqual(['a', 'c'])
    expect(j.cursor).toBe(2)
    expect(redoEntry(j, 0)).toBe(null)
  })

  it('undo on page B, pressed many times, never answers or changes an entry of page A', () => {
    const j = createJournal()
    for (const n of [0, 1, 2]) {
      push(j, `a${n}`, on(0, n))
      push(j, `b${n}`, on(1, n))
    }
    const pageA = JSON.stringify(j.entries.filter((e) => e.page === 0))
    const answered = []
    for (let n = 0; n < 6; n += 1) {
      const entry = undoEntry(j, 1)
      if (entry) answered.push([entry.label, entry.page])
    }
    expect(answered).toEqual([['b2', 1], ['b1', 1], ['b0', 1]])
    expect(nextEntry(j, 1, 'undo')).toBe(null)
    expect(JSON.stringify(j.entries.filter((e) => e.page === 0))).toBe(pageA)
    expect(nextEntry(j, 0, 'undo').label).toBe('a2')
  })

  it('a push on page B after an undo on B keeps the redo entries of page A', () => {
    const j = createJournal()
    push(j, 'a0', on(0))
    push(j, 'b0', on(1))
    push(j, 'b1', on(1, 2))
    undoEntry(j, 0)
    undoEntry(j, 1)
    push(j, 'b2', on(1, 3))
    expect(marks(j)).toEqual([['a0', 0, true], ['b0', 1, false], ['b2', 1, false]])
    expect(redoEntry(j, 1)).toBe(null)
    expect(redoEntry(j, 0).label).toBe('a0')
  })

  it('redo on page A still works after edits on page B', () => {
    const j = createJournal()
    push(j, 'a0', on(0))
    push(j, 'a1', on(0, 2))
    undoEntry(j, 0)
    undoEntry(j, 0)
    push(j, 'b0', on(1))
    push(j, 'b1', on(1, 2))
    expect(redoEntry(j, 0).label).toBe('a0')
    expect(redoEntry(j, 0).label).toBe('a1')
    expect(redoEntry(j, 0)).toBe(null)
    expect(undoEntry(j, 1).label).toBe('b1')
  })

  it('an entry with parts on two pages is a step on each', () => {
    const j = createJournal()
    const part = (regionId) => ({ regionId, before: { present: true }, after: { present: false } })
    const first = pushEntry(j, { ...delta('masks.command.deleteMask', on(0, 1)), parts: [part(on(0, 1)), part(on(1, 2)), part(on(0, 3))] }, LIMIT, pageOf)
    expect(first.seq).toBe(1)
    expect(j.entries.map((e) => [e.seq, e.page, e.regionId, (e.parts ?? []).map((p) => p.regionId)])).toEqual([
      [1, 0, on(0, 1), [on(0, 1), on(0, 3)]],
      [2, 1, on(1, 2), []],
    ])
    undoEntry(j, 1)
    expect(nextEntry(j, 1, 'undo')).toBe(null)
    expect(nextEntry(j, 0, 'undo').seq).toBe(1)
  })

  it('a detection step takes the page it is given', () => {
    const j = createJournal()
    pushEntry(j, { label: 'masks.command.detectPages', op: 'detection-state', regionId: '', before: { present: false }, after: { present: false }, pages: [], page: 4 }, LIMIT, pageOf)
    expect(j.entries[0].page).toBe(4)
  })

  it('is capped, oldest out, and never reissues a number', () => {
    for (const limit of LIMITS) {
      const j = createJournal()
      for (let n = 0; n < limit + 10; n += 1) push(j, `e${n}`, on(n % 3, n), limit)

      expect(j.entries).toHaveLength(limit)
      expect(j.cursor).toBe(limit)
      expect(j.entries[0].label).toBe('e10')
      expect(j.nextSeq).toBe(limit + 11)
      // No number came back: the last entry's seq is still its position in the
      // whole history, not in the window that survived.
      expect(j.entries.at(-1).seq).toBe(limit + 10)
      expect(viewOf(j, limit).limit).toBe(limit)
    }
  })

  it('a page whose undone entry is trimmed loses its whole redo side', () => {
    const j = createJournal()
    push(j, 'a0', on(0))
    push(j, 'a1', on(0, 2))
    undoEntry(j, 0)
    undoEntry(j, 0)
    for (let n = 0; n < 19; n += 1) push(j, `b${n}`, on(1, n), 20)
    expect(j.entries.every((e) => e.page === 1)).toBe(true)
    expect(redoEntry(j, 0)).toBe(null)
  })

  it('a lower limit trims the oldest at the next push', () => {
    const j = createJournal()
    for (let n = 0; n < 100; n += 1) push(j, `e${n}`, on(0, n), 500)
    undoEntry(j, 0)
    undoEntry(j, 0)
    push(j, 'new', on(0), 20)

    expect(j.entries).toHaveLength(20)
    expect(j.cursor).toBe(20)
    expect(j.entries[0].label).toBe('e79')
    expect(undoEntry(j, 0).label).toBe('new')
    expect(undoEntry(j, 0).label).toBe('e97')
  })

  it('a delta names a mask by reference and carries no pixels', () => {
    const j = createJournal()
    const entry = push(j, 'masks.command.deleteMask', on(0))
    expect(entry.before.region).toEqual({ id: on(0) })
    expect(entry.op).toBe('region-state')
    // The whole entry is small - this is the property the cap is sized against.
    expect(JSON.stringify(entry).length).toBeLessThan(400)
  })
})
