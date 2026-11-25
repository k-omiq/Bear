/**
 * The mock's shared data folder, so the Vite mock can walk every mode: it
 * starts in Legacy, Set up, Move and Use existing answer with Shared, and
 * Save hands a chapter over only in Shared mode, with the backend's notices.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createMockBackend } from './mock.js'

const TIMING = { method: 0, openChapter: 0, export: 0, region: 100, pageTail: 100, noticeStagger: 0 }
const CHAPTER = 'tsuki-to-hane-ch107'

/** @param {Promise<any>} promise */
async function settle(promise) {
  const settled = promise.then((value) => ({ value }), (error) => ({ error }))
  await vi.runAllTimersAsync()
  const outcome = await settled
  if ('error' in outcome) throw outcome.error
  return outcome.value
}

/** @param {ReturnType<typeof createMockBackend>} backend */
function notices(backend) {
  /** @type {any[]} */
  const seen = []
  backend.subscribe((event) => {
    if (event.type === 'notice') seen.push(event)
  })
  return seen
}

beforeEach(() => vi.useFakeTimers())
afterEach(() => vi.useRealTimers())

describe('the data folder in the mock', () => {
  it('starts in Legacy and sets the folder up', async () => {
    const backend = createMockBackend({ timing: TIMING })
    const before = await settle(backend.dataFolderStatus())
    expect(before).toMatchObject({ mode: 'legacy', root: null, defaultRoot: '/Users/reader/Manga Library' })
    const after = await settle(backend.dataFolderSetup({ target: before.defaultRoot }))
    expect(after).toMatchObject({ mode: 'shared', root: '/Users/reader/Manga Library', library: '/Users/reader/Manga Library/Cleaner' })
  })

  it('moves a shared folder, and keeps an old copy after a move to another drive', async () => {
    const backend = createMockBackend({ timing: TIMING, dataFolder: 'shared' })
    const moved = await settle(backend.dataFolderMove({ target: '/Volumes/Big/Manga Library' }))
    expect(moved.root).toBe('/Volumes/Big/Manga Library')
    expect(moved.oldCopies).toHaveLength(1)
    const cleared = await settle(backend.dataFolderDeleteOldCopy({ path: moved.oldCopies[0].path }))
    expect(cleared.oldCopies).toEqual([])
  })

  it('asks for the typesetter to be closed when it cannot tell', async () => {
    const backend = createMockBackend({ timing: TIMING, dataFolder: 'shared', typesetterRunning: 'unknown' })
    await expect(settle(backend.dataFolderMove({ target: '/Users/reader/Elsewhere' }))).rejects.toThrow(/^typesetter_unknown:/)
    const moved = await settle(backend.dataFolderMove({ target: '/Users/reader/Elsewhere', confirmedClosed: true }))
    expect(moved.root).toBe('/Users/reader/Elsewhere')
  })

  it('lists no projects while the folder is unavailable, and uses another one', async () => {
    const backend = createMockBackend({ timing: TIMING, dataFolder: 'unavailable' })
    // The backend's text, subject first: matched anywhere, never as a prefix.
    await expect(settle(backend.listProjects())).rejects.toThrow(/^library: data_folder_unavailable: /)
    const used = await settle(backend.dataFolderUseExisting({ target: '/Users/reader/Manga Library' }))
    expect(used.mode).toBe('shared')
    expect((await settle(backend.listProjects())).length).toBeGreaterThan(0)
  })

  it('refuses Save outside Shared mode, with the backend notice', async () => {
    const backend = createMockBackend({ timing: TIMING })
    const seen = notices(backend)
    const result = await settle(backend.saveToTypesetter({ chapterId: CHAPTER }))
    expect(result).toMatchObject({ status: 'refused', reasonKey: 'notice.save.needsDataFolder' })
    expect(seen.at(-1)).toMatchObject({ key: 'notice.save.needsDataFolder', tone: 'warn' })
  })

  it('saves in Shared mode, and also next to the raws when the setting asks', async () => {
    const backend = createMockBackend({ timing: TIMING, dataFolder: 'shared' })
    const seen = notices(backend)
    const saved = await settle(backend.saveToTypesetter({ chapterId: CHAPTER }))
    expect(saved.status).toBe('saved')
    expect(saved.pages).toBeGreaterThan(0)
    expect(seen.at(-1)).toMatchObject({ key: 'notice.save.sent', params: { count: saved.pages } })

    await settle(backend.writeSettings({ saveCleanedTo: 'next-to-raws' }))
    const both = await settle(backend.saveToTypesetter({ chapterId: CHAPTER }))
    expect(both.nextToRaws).toMatch(/_cleaned$/)
    expect(seen.at(-1)).toMatchObject({ key: 'notice.save.sentAndSaved', params: { count: both.pages, path: both.nextToRaws } })
  })
})
