import { describe, expect, it } from 'vitest'
import {
  adopt,
  canRedo,
  canUndo,
  createHistory,
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
const entry = (label) => ({ seq: (seq += 1), label })

describe('history', () => {
  it('starts with nothing to undo or redo', () => {
    const h = createHistory()
    expect(canUndo(h)).toBe(false)
    expect(canRedo(h)).toBe(false)
    expect(undoLabel(h)).toBe(null)
    expect(redoLabel(h)).toBe(null)
  })

  it('moves the cursor at call time and replays on the queue', async () => {
    const h = createHistory()
    const log = []
    push(h, entry('brush stroke'))
    expect(undoLabel(h)).toBe('brush stroke')

    undo(h, spyReplay(log))
    // The cursor moves synchronously so the button never lags the click.
    expect(canUndo(h)).toBe(false)
    expect(canRedo(h)).toBe(true)
    expect(redoLabel(h)).toBe('brush stroke')
    await settled(h)
    expect(log).toEqual(['undo'])

    redo(h, spyReplay(log))
    await settled(h)
    expect(log).toEqual(['undo', 'redo'])
    expect(canUndo(h)).toBe(true)
    expect(canRedo(h)).toBe(false)
  })

  it('walks several entries in order', async () => {
    const h = createHistory()
    const log = []
    push(h, entry('a'))
    push(h, entry('b'))

    undo(h, spyReplay(log))
    expect(redoLabel(h)).toBe('b')
    expect(undoLabel(h)).toBe('a')
    undo(h, spyReplay(log))
    expect(canUndo(h)).toBe(false)
    await settled(h)
    expect(log).toEqual(['undo', 'undo'])

    redo(h, spyReplay(log))
    expect(undoLabel(h)).toBe('a')
    redo(h, spyReplay(log))
    await settled(h)
    expect(canRedo(h)).toBe(false)
  })

  it('a step past either end does nothing at all', async () => {
    const h = createHistory()
    const log = []
    undo(h, spyReplay(log))
    redo(h, spyReplay(log))
    await settled(h)
    expect(log).toEqual([])
    expect(h.cursor).toBe(0)
  })

  it('pushing a new entry clears the redo stack', () => {
    const h = createHistory()
    const log = []
    push(h, entry('a'))
    undo(h, spyReplay(log))
    expect(canRedo(h)).toBe(true)

    push(h, entry('b'))
    expect(canRedo(h)).toBe(false)
    expect(h.entries.map((e) => e.label)).toEqual(['b'])
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
    undo(h, () => new Promise((r) => setTimeout(() => (order.push('b'), r()), 1)))
    undo(h, () => new Promise((r) => setTimeout(() => (order.push('a'), r()), 20)))
    await settled(h)

    expect(order).toEqual(['b', 'a'])
  })

  it('catches a rejected replay and keeps the queue running', async () => {
    const h = createHistory()
    const boom = new Error('adapter said no')
    const calls = []
    push(h, entry('a'))
    push(h, entry('b'))

    undo(h, () => Promise.reject(boom))
    undo(h, () => Promise.resolve(calls.push('a')))
    await expect(settled(h)).resolves.toBeUndefined()

    expect(h.error).toBe(boom)
    expect(calls).toEqual(['a'])
    expect(canUndo(h)).toBe(false)
    expect(canRedo(h)).toBe(true)
  })

  /*
   * The journal on disk is what numbers entries and applies the cap, so the
   * interface takes the index it is given rather than keeping its own count.
   */
  it('adopts the journal index a backend reports', () => {
    const h = createHistory()
    push(h, entry('local'))
    adopt(h, { cursor: 1, entries: [{ seq: 9, label: 'from disk' }] })
    expect(h.entries).toEqual([{ seq: 9, label: 'from disk' }])
    expect(undoLabel(h)).toBe('from disk')
    expect(canRedo(h)).toBe(false)
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
    expect(h.cursor).toBe(20)
    expect(h.entries[0].label).toBe('e12')
    expect(undoLabel(h)).toBe('new')

    // A view with no limit on it leaves the last one in force.
    adopt(h, { cursor: 1, entries: [{ seq: 1, label: 'from disk' }] })
    expect(h.limit).toBe(20)
  })

  it('adopts an empty or missing view as an empty history', () => {
    const h = createHistory()
    push(h, entry('local'))
    adopt(h, null)
    expect(h.entries).toEqual([])
    expect(canUndo(h)).toBe(false)
  })

  it('clamps a cursor the backend reports out of range', () => {
    const h = createHistory()
    adopt(h, { cursor: 99, entries: [{ seq: 1, label: 'a' }] })
    expect(h.cursor).toBe(1)
    adopt(h, { cursor: -4, entries: [{ seq: 1, label: 'a' }] })
    expect(h.cursor).toBe(0)
  })
})
