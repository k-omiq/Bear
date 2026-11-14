import { describe, expect, it } from 'vitest'
import {
  compareChapterNumbers,
  isChapterNumber,
  parseChapterNumber,
  sameChapterNumber,
} from './chapternumber.js'

describe('the chapter number rule', () => {
  it('takes split numbers and keeps them as typed', () => {
    expect(parseChapterNumber('12')).toEqual({ whole: 12, label: null, parts: [12] })
    expect(parseChapterNumber(' 12.5 ')).toEqual({ whole: 12, label: '12.5', parts: [12, 5] })
    for (const typed of ['12-1', '12/2', '12,5', '0.5', '1.2.3']) {
      expect(parseChapterNumber(typed)?.label).toBe(typed)
    }
    expect(parseChapterNumber(7)).toEqual({ whole: 7, label: null, parts: [7] })
  })

  it('refuses what the backend refuses', () => {
    for (const refused of ['', ' ', '12.', '.5', '12..5', '12/-2', '12a', '../12', '12 5', 'Ch. 12', '1234567890123456789012345']) {
      expect(parseChapterNumber(refused), refused).toBeNull()
    }
    expect(isChapterNumber('0')).toBe(false)
    expect(isChapterNumber('0.5')).toBe(true)
  })

  it('treats two spellings of one split as the same chapter', () => {
    expect(sameChapterNumber('12.5', '12-5')).toBe(true)
    expect(sameChapterNumber('012', 12)).toBe(true)
    expect(sameChapterNumber('12.5', '12.50')).toBe(false)
    expect(sameChapterNumber('12', '12.0')).toBe(false)
  })

  it('sorts naturally: 12, 12.5, 12.10, 13', () => {
    const numbers = [13, '12.10', 12, '12.5', '12-1', 2]
    expect([...numbers].sort(compareChapterNumbers)).toEqual([2, 12, '12-1', '12.5', '12.10', 13])
  })
})
