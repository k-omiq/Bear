/**
 * The start table (`docs/shared-data-folder.md` section 5.1), the stored
 * answers (5.3), and the two small readings the dialog draws with: a shown
 * path and the copy time at 20 MB/s.
 */
import { describe, expect, it } from 'vitest'
import {
  COPY_BYTES_PER_SECOND,
  copyMinutes,
  isFirstLaunch,
  readStartAnswers,
  shownPath,
  startAction,
  startRow,
} from './startflow.js'

/** @param {Record<string, unknown>} [over] */
const status = (over = {}) => /** @type {any} */ ({
  mode: 'legacy',
  root: null,
  sisterPresent: false,
  journal: false,
  titles: 0,
  ...over,
})

const fresh = { answers: {}, firstLaunchDone: false }
const usedBefore = { answers: {}, firstLaunchDone: true }

describe('the start table', () => {
  it('reads an unfinished move first, whatever else holds (row B)', () => {
    expect(startRow(status({ mode: 'unavailable', journal: true }), fresh)).toBe('B')
    expect(startRow(status({ mode: 'shared', journal: true }), fresh)).toBe('B')
    expect(startAction('B', {})).toBeNull()
  })

  it('asks nothing when the library is already in the shared folder (row A)', () => {
    expect(startRow(status({ mode: 'shared', root: '/r' }), fresh)).toBe('A')
    expect(startAction('A', {})).toBeNull()
  })

  it('offers Join inside the first-launch dialog on a first launch (row C)', () => {
    expect(startRow(status({ mode: 'pending', root: '/r' }), fresh)).toBe('C')
    expect(startAction('C', {})).toEqual({ kind: 'join' })
  })

  it('opens the Join dialog when the app was used before (row D)', () => {
    expect(startRow(status({ mode: 'pending', root: '/r' }), usedBefore)).toBe('D')
    // Titles of its own also mean "used before".
    expect(startRow(status({ mode: 'pending', root: '/r', titles: 2 }), fresh)).toBe('D')
    expect(startAction('D', {})).toEqual({ kind: 'dialog', variant: 'join' })
  })

  it('sets up quietly when Pear never ran here (row E)', () => {
    expect(startRow(status(), fresh)).toBe('E')
    expect(startAction('E', {})).toEqual({ kind: 'quiet' })
  })

  it('opens the Set up dialog when Pear ran here (row F)', () => {
    expect(startRow(status({ sisterPresent: true }), fresh)).toBe('F')
    expect(startAction('F', {})).toEqual({ kind: 'dialog', variant: 'setup' })
  })

  it('leaves an Unavailable folder that is not a move, and an unread status, as they were', () => {
    expect(startRow(status({ mode: 'unavailable' }), fresh)).toBeNull()
    expect(startRow(null, fresh)).toBeNull()
    expect(startAction(null, {})).toBeNull()
  })

  it('never asks a question again once it has an answer, either answer', () => {
    for (const how of /** @type {const} */ (['accepted', 'declined'])) {
      expect(startAction('E', { setup: how })).toBeNull()
      expect(startAction('F', { setup: how })).toBeNull()
      expect(startAction('C', { join: how })).toBeNull()
      expect(startAction('D', { join: how })).toBeNull()
    }
  })

  it('keeps Join a separate question from Set up', () => {
    expect(startAction('D', { setup: 'declined' })).toEqual({ kind: 'dialog', variant: 'join' })
    expect(startAction('F', { join: 'declined' })).toEqual({ kind: 'dialog', variant: 'setup' })
  })

  it('counts any stored answer, and a finished first-launch dialog, as not a first launch', () => {
    expect(isFirstLaunch(status(), {}, false)).toBe(true)
    expect(isFirstLaunch(status(), { setup: 'declined' }, false)).toBe(false)
    expect(isFirstLaunch(status(), {}, true)).toBe(false)
    expect(isFirstLaunch(status({ titles: 1 }), {}, false)).toBe(false)
    expect(startRow(status({ mode: 'pending', root: '/r' }), { answers: { setup: 'declined' }, firstLaunchDone: false })).toBe('D')
  })
})

describe('the stored answers', () => {
  it('reads version 1\'s boolean: a Move that worked, or Not now', () => {
    expect(readStartAnswers(true, 'shared')).toEqual({ setup: 'accepted' })
    expect(readStartAnswers(true, 'legacy')).toEqual({ setup: 'declined' })
    expect(readStartAnswers(true, 'pending')).toEqual({ setup: 'declined' })
    expect(readStartAnswers(false, 'legacy')).toEqual({})
  })

  it('reads each question on its own, and drops what it does not know', () => {
    expect(readStartAnswers({ setup: 'declined', join: 'accepted' }, 'shared')).toEqual({ setup: 'declined', join: 'accepted' })
    expect(readStartAnswers({ join: 'maybe', other: 'accepted' }, 'legacy')).toEqual({})
    expect(readStartAnswers(null, 'legacy')).toEqual({})
    expect(readStartAnswers('accepted', 'legacy')).toEqual({})
  })

  it('a version 1 Not now still holds back the quiet setup', () => {
    const answers = readStartAnswers(true, 'legacy')
    expect(startAction(startRow(status(), { answers, firstLaunchDone: true }), answers)).toBeNull()
  })
})

describe('shown paths', () => {
  it('drops the Windows long-path prefix, the UNC one too', () => {
    expect(shownPath('\\\\?\\D:\\Manga Library')).toBe('D:\\Manga Library')
    expect(shownPath('\\\\?\\UNC\\nas\\share\\Manga Library')).toBe('\\\\nas\\share\\Manga Library')
    expect(shownPath('D:\\Manga Library')).toBe('D:\\Manga Library')
  })

  it('reads the home folder as ~ on macOS and Linux, and only a whole folder', () => {
    expect(shownPath('/Users/reader/Manga Library', '/Users/reader')).toBe('~/Manga Library')
    expect(shownPath('/Users/reader', '/Users/reader/')).toBe('~')
    expect(shownPath('/Users/readerX/Manga Library', '/Users/reader')).toBe('/Users/readerX/Manga Library')
    expect(shownPath('/Volumes/Manga/Manga Library', '/Users/reader')).toBe('/Volumes/Manga/Manga Library')
    expect(shownPath('C:\\Users\\reader\\Manga Library', 'C:\\Users\\reader')).toBe('C:\\Users\\reader\\Manga Library')
    expect(shownPath(null)).toBe('')
  })
})

describe('the copy time', () => {
  it('is the size at 20 MB/s, rounded up to whole minutes, at least one', () => {
    expect(COPY_BYTES_PER_SECOND).toBe(20_000_000)
    expect(copyMinutes(0)).toBe(1)
    expect(copyMinutes(1)).toBe(1)
    expect(copyMinutes(1_200_000_000)).toBe(1)
    expect(copyMinutes(1_200_000_001)).toBe(2)
    // The contract's example: 9.7 GB is 485 s, so 9 minutes rounded up.
    expect(copyMinutes(9_700_000_000)).toBe(9)
  })
})
