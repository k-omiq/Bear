import { describe, expect, it } from 'vitest'
import {
  createJournal,
  pushEntry,
  redoEntry,
  undoEntry,
  viewOf,
} from './journal.js'

/*
 * These are the same assertions `history::tests` makes in Rust, against the
 * same three rules. Two implementations run in two processes - the core owns a
 * file, the mock owns a fixture library with no file - and the point of writing
 * the tests twice is that the rules are the contract, not the storage.
 */

/** The limit is the backend's to choose; these are its lowest, default and highest. */
const LIMITS = [20, 500, 2000]
const LIMIT = 500

const delta = (label, regionId = 'r1') => ({
  label,
  op: 'region-state',
  regionId,
  before: { present: true, pageStatus: 'cleaned', region: { id: regionId } },
  after: { present: false, pageStatus: 'unclean', region: null },
})

describe('journal', () => {
  it('numbers an entry and puts the cursor at the end', () => {
    const j = createJournal()
    expect(pushEntry(j, delta('a'), LIMIT).seq).toBe(1)
    expect(pushEntry(j, delta('b'), LIMIT).seq).toBe(2)
    expect(j.cursor).toBe(2)
  })

  it('the view carries labels and no payload', () => {
    const j = createJournal()
    pushEntry(j, delta('masks.command.deleteMask'), LIMIT)
    const view = viewOf(j, LIMIT)
    expect(view).toEqual({ limit: LIMIT, cursor: 1, entries: [{ seq: 1, label: 'masks.command.deleteMask' }] })
    expect(JSON.stringify(view)).not.toContain('region-state')
  })

  it('undo and redo walk the same entries', () => {
    const j = createJournal()
    pushEntry(j, delta('a'), LIMIT)
    pushEntry(j, delta('b'), LIMIT)

    expect(undoEntry(j).label).toBe('b')
    expect(undoEntry(j).label).toBe('a')
    expect(undoEntry(j)).toBe(null)
    expect(j.cursor).toBe(0)

    expect(redoEntry(j).label).toBe('a')
    expect(redoEntry(j).label).toBe('b')
    expect(redoEntry(j)).toBe(null)
  })

  it('a push after an undo drops the future', () => {
    const j = createJournal()
    pushEntry(j, delta('a'), LIMIT)
    pushEntry(j, delta('b'), LIMIT)
    undoEntry(j)
    pushEntry(j, delta('c'), LIMIT)

    expect(j.entries.map((e) => e.label)).toEqual(['a', 'c'])
    expect(j.cursor).toBe(2)
    expect(redoEntry(j)).toBe(null)
  })

  it('is capped, oldest out, and never reissues a number', () => {
    for (const limit of LIMITS) {
      const j = createJournal()
      for (let n = 0; n < limit + 10; n += 1) pushEntry(j, delta(`e${n}`), limit)

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

  it('a lower limit trims the oldest at the next push', () => {
    const j = createJournal()
    for (let n = 0; n < 100; n += 1) pushEntry(j, delta(`e${n}`), 500)
    undoEntry(j)
    undoEntry(j)
    pushEntry(j, delta('new'), 20)

    expect(j.entries).toHaveLength(20)
    expect(j.cursor).toBe(20)
    expect(j.entries[0].label).toBe('e79')
    expect(undoEntry(j).label).toBe('new')
    expect(undoEntry(j).label).toBe('e97')
  })

  it('a delta names a mask by reference and carries no pixels', () => {
    const j = createJournal()
    const entry = pushEntry(j, delta('masks.command.deleteMask'), LIMIT)
    expect(entry.before.region).toEqual({ id: 'r1' })
    expect(entry.op).toBe('region-state')
    // The whole entry is small - this is the property the cap is sized against.
    expect(JSON.stringify(entry).length).toBeLessThan(400)
  })
})
