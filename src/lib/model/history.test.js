import { describe, expect, it } from 'vitest'
import {
  adopt,
  busy,
  canRedo,
  canUndo,
  createHistory,
  enqueue,
  push,
  redo,
  redoLabel,
  settled,
  undo,
  undoLabel,
} from './history.js'

/** A replay that records the direction it was asked for, in order. */
function spyReplay(log, delays = {}) {
  return (direction) =>
    new Promise((resolve) =>
      setTimeout(() => {
        log.push(direction)
        resolve()
      }, delays[direction] ?? 0),
    )
}

let seq = 0
/** An entry on `page`; page 0 unless said. */
const entry = (label, page = 0) => ({ seq: (seq += 1), label, page })
const labelsOf = (h, page) => h.entries.filter((e) => e.page === page).map((e) => [e.label, e.undone])

describe('history', () => {
  it('starts with nothing to undo or redo', () => {
    const h = createHistory()
    expect(canUndo(h, 0)).toBe(false)
    expect(canRedo(h, 0)).toBe(false)
    expect(undoLabel(h, 0)).toBe(null)
    expect(redoLabel(h, 0)).toBe(null)
  })

  it('moves the mark at call time and replays on the queue', async () => {
    const h = createHistory()
    const log = []
    push(h, entry('brush stroke'))
    expect(undoLabel(h, 0)).toBe('brush stroke')

    undo(h, 0, spyReplay(log))
    // The mark moves synchronously so the button never lags the click.
    expect(canUndo(h, 0)).toBe(false)
    expect(canRedo(h, 0)).toBe(true)
    expect(redoLabel(h, 0)).toBe('brush stroke')
    await settled(h)
    expect(log).toEqual(['undo'])

    redo(h, 0, spyReplay(log))
    await settled(h)
    expect(log).toEqual(['undo', 'redo'])
    expect(canUndo(h, 0)).toBe(true)
    expect(canRedo(h, 0)).toBe(false)
  })

  it('walks several entries in order', async () => {
    const h = createHistory()
    const log = []
    push(h, entry('a'))
    push(h, entry('b'))

    undo(h, 0, spyReplay(log))
    expect(redoLabel(h, 0)).toBe('b')
    expect(undoLabel(h, 0)).toBe('a')
    undo(h, 0, spyReplay(log))
    expect(canUndo(h, 0)).toBe(false)
    await settled(h)
    expect(log).toEqual(['undo', 'undo'])

    redo(h, 0, spyReplay(log))
    expect(undoLabel(h, 0)).toBe('a')
    expect(redoLabel(h, 0)).toBe('b')
    redo(h, 0, spyReplay(log))
    await settled(h)
    expect(canRedo(h, 0)).toBe(false)
  })

  it('a step past either end does nothing at all', async () => {
    const h = createHistory()
    const log = []
    undo(h, 0, spyReplay(log))
    redo(h, 0, spyReplay(log))
    await settled(h)
    expect(log).toEqual([])
    expect(h.revision).toBe(0)
  })

  it('pushing a new entry clears the redo stack of its page', () => {
    const h = createHistory()
    const log = []
    push(h, entry('a'))
    undo(h, 0, spyReplay(log))
    expect(canRedo(h, 0)).toBe(true)

    push(h, entry('b'))
    expect(canRedo(h, 0)).toBe(false)
    expect(h.entries.map((e) => e.label)).toEqual(['b'])
  })

  /*
   * Undo and redo never leave the page on screen: the rule the chapter-wide
   * stack broke, when Undo on page 14 walked back through work on pages 2, 6,
   * 9 and 10 and the next edit cut it off.
   */
  describe('one page at a time', () => {
    it('undo on page B, pressed many times, never answers or changes an entry of page A', async () => {
      const h = createHistory()
      const asked = []
      for (const n of [0, 1, 2]) {
        push(h, entry(`a${n}`, 0))
        push(h, entry(`b${n}`, 1))
      }
      const pageA = JSON.stringify(h.entries.filter((e) => e.page === 0))
      for (let n = 0; n < 6; n += 1) undo(h, 1, (direction, page, picked) => { asked.push([direction, page, picked.label]) })
      await settled(h)
      expect(asked).toEqual([['undo', 1, 'b2'], ['undo', 1, 'b1'], ['undo', 1, 'b0']])
      expect(canUndo(h, 1)).toBe(false)
      expect(JSON.stringify(h.entries.filter((e) => e.page === 0))).toBe(pageA)
      expect(undoLabel(h, 0)).toBe('a2')
      expect(canRedo(h, 0)).toBe(false)
    })

    it('a push on page B after an undo on B keeps the redo entries of page A', () => {
      const h = createHistory()
      push(h, entry('a0', 0))
      push(h, entry('b0', 1))
      push(h, entry('b1', 1))
      undo(h, 0, () => {})
      undo(h, 1, () => {})
      push(h, entry('b2', 1))
      expect(labelsOf(h, 0)).toEqual([['a0', true]])
      expect(labelsOf(h, 1)).toEqual([['b0', false], ['b2', false]])
      expect(redoLabel(h, 0)).toBe('a0')
      expect(canRedo(h, 1)).toBe(false)
    })

    it('redo on page A still works after edits on page B', () => {
      const h = createHistory()
      push(h, entry('a0', 0))
      push(h, entry('a1', 0))
      undo(h, 0, () => {})
      undo(h, 0, () => {})
      push(h, entry('b0', 1))
      push(h, entry('b1', 1))
      expect(redoLabel(h, 0)).toBe('a0')
      redo(h, 0, () => {})
      expect(redoLabel(h, 0)).toBe('a1')
      redo(h, 0, () => {})
      expect(canRedo(h, 0)).toBe(false)
      expect(undoLabel(h, 1)).toBe('b1')
    })

    it('a run over two pages is a step on each, as the backend reports it', () => {
      const h = createHistory()
      adopt(h, { limit: 500, cursor: 2, entries: [
        { seq: 1, label: 'masks.command.detectPages', page: 0, undone: false },
        { seq: 2, label: 'masks.command.detectPages', page: 1, undone: false },
      ] })
      undo(h, 1, () => {})
      expect(canUndo(h, 1)).toBe(false)
      expect(undoLabel(h, 0)).toBe('masks.command.detectPages')
      expect(redoLabel(h, 1)).toBe('masks.command.detectPages')
    })

    it('asks for no page it is not given', () => {
      const h = createHistory()
      push(h, entry('a', 0))
      expect(canUndo(h, null)).toBe(false)
      expect(canUndo(h, undefined)).toBe(false)
      expect(canUndo(h, 1)).toBe(false)
    })
  })

  /*
   * Replays go through the backend, so they are async. Two fast undos must not
   * race: the second starts only once the first has finished, or the two
   * adapter calls settle in whichever order the disk felt like and the later
   * one wins.
   */
  it('runs replays one at a time, in order', async () => {
    const h = createHistory()
    const order = []
    push(h, entry('a'))
    push(h, entry('b'))

    // The quick one is asked for first and must still finish first.
    undo(h, 0, () => new Promise((r) => setTimeout(() => (order.push('b'), r()), 1)))
    undo(h, 0, () => new Promise((r) => setTimeout(() => (order.push('a'), r()), 20)))
    await settled(h)

    expect(order).toEqual(['b', 'a'])
  })

  it('catches a rejected replay and keeps the queue running', async () => {
    const h = createHistory()
    const boom = new Error('adapter said no')
    const calls = []
    push(h, entry('a'))
    push(h, entry('b'))

    undo(h, 0, () => Promise.reject(boom))
    undo(h, 0, () => Promise.resolve(calls.push('a')))
    await expect(settled(h)).resolves.toBeUndefined()

    expect(h.error).toBe(boom)
    expect(calls).toEqual(['a'])
    expect(canUndo(h, 0)).toBe(false)
    expect(canRedo(h, 0)).toBe(true)
  })

  /*
   * The journal on disk is what numbers entries and applies the cap, so the
   * interface takes the index it is given rather than keeping its own count.
   */
  it('adopts the journal index a backend reports', () => {
    const h = createHistory()
    push(h, entry('local'))
    adopt(h, { cursor: 1, entries: [{ seq: 9, label: 'from disk', page: 3, undone: false }] })
    expect(h.entries).toEqual([{ seq: 9, label: 'from disk', page: 3, undone: false }])
    expect(undoLabel(h, 3)).toBe('from disk')
    expect(canUndo(h, 0)).toBe(false)
    expect(canRedo(h, 3)).toBe(false)
  })

  /*
   * The cap is the backend's number. Until a view has carried it the index is
   * not capped at all, and after that a local push holds to it.
   */
  it('caps a local push at the limit the backend reported', () => {
    const h = createHistory()
    for (let n = 1; n <= 30; n += 1) push(h, entry(`e${n}`))
    expect(h.entries).toHaveLength(30)

    adopt(h, { limit: 20, cursor: 20, entries: h.entries.slice(10) })
    push(h, entry('new'))
    expect(h.entries).toHaveLength(20)
    expect(h.entries.every((e) => !e.undone)).toBe(true)
    expect(h.entries[0].label).toBe('e12')
    expect(undoLabel(h, 0)).toBe('new')

    // A view with no limit on it leaves the last one in force.
    adopt(h, { cursor: 1, entries: [{ seq: 1, label: 'from disk' }] })
    expect(h.limit).toBe(20)
  })

  it('a page whose undone entry is trimmed loses its whole redo side', () => {
    const h = createHistory()
    adopt(h, { limit: 3, cursor: 0, entries: [] })
    push(h, entry('a0', 0))
    push(h, entry('a1', 0))
    undo(h, 0, () => {})
    undo(h, 0, () => {})
    push(h, entry('b0', 1))
    push(h, entry('b1', 1))
    expect(h.entries.map((e) => e.label)).toEqual(['b0', 'b1'])
    expect(canRedo(h, 0)).toBe(false)
  })

  it('adopts an empty or missing view as an empty history', () => {
    const h = createHistory()
    push(h, entry('local'))
    adopt(h, null)
    expect(h.entries).toEqual([])
    expect(canUndo(h, 0)).toBe(false)
  })

  it('reads a view without marks by its cursor, clamped', () => {
    const h = createHistory()
    adopt(h, { cursor: 1, entries: [{ seq: 1, label: 'a', page: 0 }, { seq: 2, label: 'b', page: 0 }] })
    expect(h.entries.map((e) => e.undone)).toEqual([false, true])
    adopt(h, { cursor: 99, entries: [{ seq: 1, label: 'a', page: 0 }] })
    expect(h.entries[0].undone).toBe(false)
    adopt(h, { cursor: -4, entries: [{ seq: 1, label: 'a', page: 0 }] })
    expect(h.entries[0].undone).toBe(true)
  })
})

describe('the one queue', () => {
  it('starts a task at once when idle, and queues the rest in order', async () => {
    const h = createHistory()
    const log = []
    let release = () => {}
    enqueue(h, () => new Promise((resolve) => { log.push('push'); release = resolve }))
    // Started in the caller's turn: nothing was ahead of it.
    expect(log).toEqual(['push'])
    expect(busy(h)).toBe(true)
    push(h, { seq: 1, label: 'a', page: 0 })
    undo(h, 0, (direction) => { log.push(direction) })
    enqueue(h, () => { log.push('read') })
    await Promise.resolve()
    expect(log).toEqual(['push'])
    release()
    await settled(h)
    expect(log).toEqual(['push', 'undo', 'read'])
    expect(busy(h)).toBe(false)
  })

  it('keeps a failure and goes on with the tasks behind it', async () => {
    const h = createHistory()
    const log = []
    enqueue(h, () => { throw new Error('first') })
    enqueue(h, () => { log.push('second') })
    await settled(h)
    expect(log).toEqual(['second'])
    expect(h.error).toBeInstanceOf(Error)
    expect(h.pending).toBe(0)
  })
})
