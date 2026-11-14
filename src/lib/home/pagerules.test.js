/**
 * The Pages screen's rule, said again from `library.rs#check_page_order`: a
 * page with work keeps its place, a longstrip with work only grows at its end,
 * a chapter keeps one page. The Rust tests hold the backend to it; these hold
 * the screen to the same answers.
 */

import { describe, expect, it } from 'vitest'

import {
  insertRefusal,
  moveRefusal,
  orderMovingTo,
  orderStepping,
  pageState,
  positionOfSource,
  removalLoss,
  removeRefusal,
} from './pagerules.js'

/**
 * @param {boolean[]} worked
 * @param {{longstrip?: boolean, busy?: boolean}} [extra]
 */
function listing(worked, extra = {}) {
  return /** @type {any} */ ({
    chapterId: 'c1',
    longstrip: false,
    busy: false,
    pages: worked.map((_, i) => ({ id: `p${i}`, sourceIndex: i, file: `${i}.png` })),
    worked,
    removed: [],
    ...extra,
  })
}

describe('removing pages', () => {
  it('takes pages after the last page with work, and refuses ones before it', () => {
    const pages = listing([false, true, false, false])
    expect(removeRefusal(pages, [2, 3])).toBeNull()
    expect(removeRefusal(pages, [0])).toEqual({ key: 'home.pagesScreen.why.pinned', params: { page: 2 } })
  })

  it('takes a page with work itself, and the work goes with it', () => {
    const pages = listing([false, true, false])
    expect(removeRefusal(pages, [1, 2])).toBeNull()
    expect(removalLoss(pages, [1, 2])).toEqual({ pages: 2, worked: 1 })
  })

  it('keeps one page, and says so while a job holds the chapter', () => {
    expect(removeRefusal(listing([false, false]), [0, 1])).toEqual({ key: 'home.pagesScreen.why.lastPage', params: {} })
    expect(removeRefusal(listing([false, false], { busy: true }), [1])).toEqual({ key: 'home.pagesScreen.why.busy', params: {} })
  })
})

describe('adding pages', () => {
  it('goes anywhere after the last page with work', () => {
    const pages = listing([true, false, false])
    expect(insertRefusal(pages, 3)).toBeNull()
    expect(insertRefusal(pages, 1)).toBeNull()
    expect(insertRefusal(pages, 0)).toEqual({ key: 'home.pagesScreen.why.pinned', params: { page: 1 } })
  })

  it('only goes at the end of a longstrip with work', () => {
    const strip = listing([true, false], { longstrip: true })
    expect(insertRefusal(strip, 2)).toBeNull()
    expect(insertRefusal(strip, 1)).toEqual({ key: 'home.pagesScreen.why.longstrip', params: {} })
    expect(insertRefusal(listing([false, false], { longstrip: true }), 0)).toBeNull()
  })
})

describe('moving pages', () => {
  it('moves the selection as one block, in its own order', () => {
    expect(orderMovingTo(5, [3, 1], 0)).toEqual([1, 3, 0, 2, 4])
    expect(orderMovingTo(5, [0], 5)).toEqual([1, 2, 3, 4, 0])
    expect(orderMovingTo(4, [1, 2], 2)).toEqual([0, 1, 2, 3])
  })

  it('steps one place, and stops at either end', () => {
    expect(orderStepping(4, [2], -1)).toEqual([0, 2, 1, 3])
    expect(orderStepping(4, [1, 2], 1)).toEqual([0, 3, 1, 2])
    expect(orderStepping(4, [0], -1)).toBeNull()
    expect(orderStepping(4, [3], 1)).toBeNull()
    expect(orderStepping(4, [], 1)).toBeNull()
  })

  it('refuses an order that moves a page with work', () => {
    const pages = listing([false, false, true, false, false])
    expect(moveRefusal(pages, [1, 0, 2, 3, 4])).toBeNull()
    expect(moveRefusal(pages, [0, 1, 2, 4, 3])).toBeNull()
    expect(moveRefusal(pages, [0, 2, 1, 3, 4])).toEqual({ key: 'home.pagesScreen.why.pinned', params: { page: 3 } })
  })
})

describe('what the screen shows', () => {
  it('names each page by the furthest it has come', () => {
    const page = /** @type {any} */ ({})
    expect(pageState({ ...page, status: 'cleaned' }, true)).toBe('cleaned')
    expect(pageState({ ...page, doneCount: 1 }, true)).toBe('cleaned')
    expect(pageState({ ...page, regionCount: 3 }, true)).toBe('masks')
    expect(pageState(page, true)).toBe('edited')
    expect(pageState(page, false)).toBe('untouched')
  })

  it('follows a page by its source, or stays as near as the chapter allows', () => {
    const pages = [{ sourceIndex: 4 }, { sourceIndex: 0 }, { sourceIndex: 2 }]
    expect(positionOfSource(pages, 0, 0)).toBe(1)
    expect(positionOfSource(pages, 9, 5)).toBe(2)
    expect(positionOfSource(pages, undefined, 1)).toBe(1)
  })
})
