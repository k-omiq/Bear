/**
 * The mock's page edits keep the backend's rule: pages are added, removed,
 * moved and put back, and a page with work keeps its place.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createMockBackend } from './mock.js'

const TIMING = { method: 0, openChapter: 0, export: 0, region: 100, pageTail: 100, noticeStagger: 0 }

/** 48 pages with no text, so no page has work. */
const PLAIN = 'yoake-photobook-ch1'
/** Its pages carry regions, so every page has work. */
const WORKED = 'tsuki-to-hane-ch107'

/** @param {Promise<any>} promise */
async function settle(promise) {
  await vi.runAllTimersAsync()
  return promise
}

beforeEach(() => vi.useFakeTimers())
afterEach(() => vi.useRealTimers())

describe('page edits in the mock', () => {
  it('adds images at the end and skips other files', async () => {
    const backend = createMockBackend({ timing: TIMING })
    const before = await settle(backend.chapterPages({ chapterId: PLAIN }))
    const after = await settle(backend.addPages({ chapterId: PLAIN, paths: ['/in/b10.png', '/in/b9.png', '/in/notes.txt'] }))
    expect(after.pages).toHaveLength(before.pages.length + 2)
    expect(after.pages.slice(-2).map((page) => page.file)).toEqual(['b9.png', 'b10.png'])
  })

  it('removes a page, keeps it to put back, and moves pages without work', async () => {
    const backend = createMockBackend({ timing: TIMING })
    const start = await settle(backend.chapterPages({ chapterId: PLAIN }))
    const first = start.pages[0].sourceIndex
    const removed = await settle(backend.removePages({ chapterId: PLAIN, indices: [0] }))
    expect(removed.pages).toHaveLength(start.pages.length - 1)
    expect(removed.removed.map((page) => page.sourceIndex)).toEqual([first])

    const back = await settle(backend.restorePages({ chapterId: PLAIN, sources: [first] }))
    expect(back.removed).toEqual([])
    expect(back.pages.at(-1).sourceIndex).toBe(first)

    const order = back.pages.map((_, i) => i)
    ;[order[0], order[1]] = [order[1], order[0]]
    const moved = await settle(backend.movePages({ chapterId: PLAIN, order }))
    expect(moved.pages[0].sourceIndex).toBe(back.pages[1].sourceIndex)
  })

  it('refuses to move a page with work, and adds after it', async () => {
    const backend = createMockBackend({ timing: TIMING })
    const start = await settle(backend.chapterPages({ chapterId: WORKED }))
    expect(start.worked[0]).toBe(true)
    const order = start.pages.map((_, i) => i)
    ;[order[0], order[1]] = [order[1], order[0]]
    expect(await settle(backend.movePages({ chapterId: WORKED, order }))).toBeNull()
    expect(start.worked[1]).toBe(true)
    expect(await settle(backend.removePages({ chapterId: WORKED, indices: [0] }))).toBeNull()
    const grown = await settle(backend.addPages({ chapterId: WORKED, paths: ['/in/new.png'] }))
    expect(grown.pages).toHaveLength(start.pages.length + 1)
  })
})
